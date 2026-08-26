//! Доменная обёртка решателя ставки (§6.1).
//!
//! Здесь живёт то, что решатель знать не должен: границы контура, валюты,
//! курсы, цены и знаковая конвенция. Сам решатель работает с парами
//! «смещение в днях, сумма» и о портфеле ничего не знает.

use std::collections::BTreeMap;

use time::Date;

use super::{Computed, NotComputable, ReturnsRequest};
use crate::ids::AccountId;
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

/// Стоимость счёта, разложенная на деньги и бумаги.
///
/// Разложение существенно для покрытия NAV (§10.5): деньги
/// подтверждаются измерением `cash`, бумаги — измерением `positions`,
/// и это **разные** утверждения. Одна цифра на счёт заставила бы брать
/// худшее из двух, и тогда счёт без единой бумаги никогда не стал бы
/// подтверждённым: измерение `positions`, о котором нечего утверждать,
/// вечно тянуло бы его вниз.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AccountValue {
    pub cash: Dec,
    pub positions: Dec,
}

impl Default for AccountValue {
    /// Нулевые части пишутся руками: `Dec` намеренно не имеет
    /// умолчания, потому что нулевая заглушка вместо неизвестной
    /// величины запрещена (§4.9). Здесь ноль осмыслен — накопитель
    /// начинает с него для счёта, который уже признан существующим.
    fn default() -> Self {
        Self {
            cash: Dec::zero(),
            positions: Dec::zero(),
        }
    }
}

impl AccountValue {
    /// Стоимость счёта целиком.
    pub fn total(&self) -> Result<Dec, NotComputable> {
        add(self.cash, self.positions)
    }
}

/// Стоимость контура **по счетам** на дату отчёта: деньги плюс позиции
/// по последней цене, наблюдавшейся не позже даты отчёта.
///
/// Это **ликвидационная** оценка в упрощённом виде (§5.1): комиссий
/// закрытия и налога к уплате в ней нет, потому что ни того, ни другого
/// этап 1 не считает. Разрыв с `contractual_hold_value` не вычисляется —
/// вклады и облигации целиком относятся к E3.
///
/// Разбиение по счетам существует потому, что покрытие NAV по уровням
/// достоверности (§10.5) взвешивается стоимостью счёта: доля,
/// посчитанная по числу записей, объявила бы счёт с одной сделкой на
/// миллион равным счёту с сотней сделок на тысячу.
pub fn account_values(
    state: &LedgerState,
    request: &ReturnsRequest,
) -> Result<BTreeMap<AccountId, AccountValue>, NotComputable> {
    guard_state_not_newer(state, request.as_of)?;
    let mut values: BTreeMap<AccountId, AccountValue> = BTreeMap::new();

    for (account, money) in state.balances().iter_cash() {
        if !request.contour.contains(account) {
            continue;
        }
        let converted = convert(money, request.report_currency, request.as_of, request.fx)?;
        let slot = values.entry(account).or_default();
        slot.cash = add(slot.cash, converted)?;
    }

    for (key, quantity) in state.balances().iter_positions() {
        if !request.contour.contains(key.account) || quantity.0.is_zero() {
            continue;
        }
        let price = state
            .prices()
            .price_at_or_before(key.instrument, request.as_of)
            .ok_or(NotComputable::MissingPrice {
                instrument: key.instrument,
            })?;
        let local = mul(quantity.0, price.price)?;
        let converted = in_report_currency(local, price.currency, request)?;
        let slot = values.entry(key.account).or_default();
        slot.positions = add(slot.positions, converted)?;
    }
    Ok(values)
}

/// Стоимость контура на дату отчёта — сумма по счетам.
pub fn terminal_value(state: &LedgerState, request: &ReturnsRequest) -> Result<Dec, NotComputable> {
    let mut total = Dec::zero();
    for value in account_values(state, request)?.values() {
        total = add(total, value.total()?)?;
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
#[cfg(test)]
mod tests {
    use super::*;
    use crate::contour::{ContourDefinition, ContourId, ContourVersion};
    use crate::event::kind::{EventKind, TradeSide};
    use crate::event::leg::Leg;
    use crate::event::test_support::event_with;
    use crate::ids::{AccountId, CustodyId, InstrumentId};
    use crate::money::{CurrencyCode, Money, PostedMinor, Quantity};
    use crate::numeric::approx::SolverPolicy;
    use crate::perimeter::{PerimeterAssessment, PerimeterPolicy};
    use crate::projection::{ProjectionContext, project};
    use crate::reconciliation::ReconciliationLedger;
    use crate::returns::KnowledgeCoordinate;
    use crate::rules::{LotRuleVersion, RuleRegistry};
    use crate::valuation::{FxSource, FxTable};
    use time::macros::date;

    fn rub(minor: i64) -> Money {
        Money::new(PostedMinor::new(minor), CurrencyCode::Rub)
    }

    fn qty(units: i64) -> Quantity {
        Quantity(Dec::new(rust_decimal::Decimal::from(units)))
    }

    #[test]
    fn account_values_ignore_outside_positions_and_zero_inside_positions() {
        let inside = AccountId::new_random();
        let outside = AccountId::new_random();
        let instrument = InstrumentId::new_random();
        let custody = CustodyId::new_random();
        let projection_contour = ContourDefinition::new(
            ContourId::new_random(),
            ContourVersion(1),
            [inside, outside],
        );
        let requested_contour =
            ContourDefinition::new(ContourId::new_random(), ContourVersion(1), [inside]);
        let rules = RuleRegistry::with_defaults();
        let context = ProjectionContext {
            contour: &projection_contour,
            rules: &rules,
            lot_rule: LotRuleVersion(1),
        };
        let events = vec![
            event_with(
                outside,
                date!(2026 - 01 - 01),
                1,
                EventKind::Trade {
                    side: TradeSide::Buy,
                    instrument,
                    quantity: qty(10),
                    gross: rub(1_000),
                    fee: None,
                    accrued_interest: None,
                },
                vec![
                    Leg::cash(outside, rub(-1_000)),
                    Leg::security(outside, custody, instrument, qty(10)),
                ],
            ),
            event_with(
                inside,
                date!(2026 - 01 - 02),
                2,
                EventKind::Trade {
                    side: TradeSide::Buy,
                    instrument,
                    quantity: qty(10),
                    gross: rub(1_000),
                    fee: None,
                    accrued_interest: None,
                },
                vec![
                    Leg::cash(inside, rub(-1_000)),
                    Leg::security(inside, custody, instrument, qty(10)),
                ],
            ),
            event_with(
                inside,
                date!(2026 - 01 - 03),
                3,
                EventKind::Trade {
                    side: TradeSide::Sell,
                    instrument,
                    quantity: qty(10),
                    gross: rub(1_000),
                    fee: None,
                    accrued_interest: None,
                },
                vec![
                    Leg::cash(inside, rub(1_000)),
                    Leg::security(inside, custody, instrument, qty(-10)),
                ],
            ),
        ];
        let snapshot = project(&events, &context)
            .expect("проекция")
            .into_snapshot();
        let ledger = ReconciliationLedger::default();
        let perimeter = PerimeterAssessment::empty(PerimeterPolicy::default());
        let fx = FxTable::new(FxSource::OwnerSupplied);
        let request = ReturnsRequest {
            contour: &requested_contour,
            coordinate: KnowledgeCoordinate::default(),
            as_of: date!(2026 - 01 - 04),
            report_currency: CurrencyCode::Rub,
            fx: &fx,
            solver_policy: SolverPolicy::returns_default(),
            ledger: &ledger,
            perimeter: &perimeter,
        };

        let values = account_values(snapshot.state(), &request).expect("стоимости счетов");
        assert_eq!(
            values.get(&inside),
            Some(&AccountValue {
                cash: Dec::zero(),
                positions: Dec::zero(),
            })
        );
        assert!(!values.contains_key(&outside));
    }
}
