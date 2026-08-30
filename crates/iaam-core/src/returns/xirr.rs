//! Domain wrapper around the rate solver (§6.1).
//!
//! This is where solver-unaware concerns live: contour boundaries, currencies,
//! rates, prices, and sign convention. The solver itself works with
//! “day offset, amount” pairs and knows nothing about portfolios.

use std::collections::BTreeMap;

use time::Date;

use super::{Computed, NotComputable, ReturnsRequest};
use crate::ids::AccountId;
use crate::numeric::decimal::Dec;
use crate::numeric::xirr::{DayCount, RateOutcome, SolverFlow, solve};
use crate::projection::flows::FlowDirection;
use crate::projection::state::LedgerState;
use crate::valuation::convert;

/// Flow series in the reporting currency.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FlowSeries {
    /// Contributed over the entire history, as a positive amount.
    pub contributed: Dec,
    /// Withdrawn over the entire history, as a positive amount.
    pub withdrawn: Dec,
    /// Dated amounts **in the owner’s sign convention**: contributions are
    /// negative and withdrawals positive. This negates cash movement across
    /// the contour: an inflow for the contour is an outflow for the owner.
    pub flows: Vec<(Date, Dec)>,
}

/// Convert external flows to the reporting currency.
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

/// Account value split into cash and securities.
///
/// The split matters for NAV coverage (§10.5): cash
/// is confirmed by the `cash` measurement, securities by `positions`,
/// and these are **different** claims. One account-level number would force
/// the worse of the two, so an account with no securities could never become
/// confirmed: the empty `positions` measurement, which has nothing to assert,
/// would drag it down forever.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AccountValue {
    pub cash: Dec,
    pub positions: Dec,
}

impl Default for AccountValue {
    /// Zero parts are written explicitly: `Dec` deliberately has no default,
    /// because a zero placeholder for an unknown
    /// value is forbidden (§4.9). Here zero is meaningful: the accumulator
    /// starts there for an account already known to exist.
    fn default() -> Self {
        Self {
            cash: Dec::zero(),
            positions: Dec::zero(),
        }
    }
}

impl AccountValue {
    /// Total account value.
    pub fn total(&self) -> Result<Dec, NotComputable> {
        add(self.cash, self.positions)
    }
}

/// Contour value **by account** on the report date: cash plus positions
/// at the price selected by valuation policy.
///
/// This is a simplified **liquidation** estimate (§5.1): closing fees
/// and tax are absent because stage 1 calculates neither. The gap to
/// `contractual_hold_value` is not calculated—
/// deposits and bonds belong wholly to E3.
///
/// Per-account splitting exists because NAV coverage by confidence level
/// (§10.5) is weighted by account value: weighting by record count
/// would make an account with one million-value trade
/// equal to one with a hundred thousand-value trades.
pub fn account_values(
    state: &LedgerState,
    request: &ReturnsRequest,
) -> Result<BTreeMap<AccountId, AccountValue>, NotComputable> {
    let positions = super::position_values(state, request);
    account_values_from_position_values(state, request, &positions)
}

pub(super) fn account_values_from_position_values(
    state: &LedgerState,
    request: &ReturnsRequest,
    positions: &[super::PositionValue],
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

    for position in positions {
        let value = match &position.value {
            Ok(value) => *value,
            Err(reason) => return Err(reason.clone()),
        };
        let slot = values.entry(position.assessment.account).or_default();
        slot.positions = add(slot.positions, value)?;
    }
    Ok(values)
}

pub(super) fn terminal_value_from_position_values(
    state: &LedgerState,
    request: &ReturnsRequest,
    positions: &[super::PositionValue],
) -> Result<Dec, NotComputable> {
    let mut total = Dec::zero();
    for value in account_values_from_position_values(state, request, positions)?.values() {
        total = add(total, value.total()?)?;
    }
    Ok(total)
}

/// Contour value on the report date—the sum by account.
pub fn terminal_value(state: &LedgerState, request: &ReturnsRequest) -> Result<Dec, NotComputable> {
    let positions = super::position_values(state, request);
    terminal_value_from_position_values(state, request, &positions)
}

/// Rate from a flow series and terminal value.
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

/// State must be projected **through the report date**: the shell filters the
/// journal while building the slice, not the core. An event after the report
/// date means the slice was assembled incorrectly; silently calculating from it
/// would produce a report for a date that did not exist on that date.
fn guard_state_not_newer(state: &LedgerState, as_of: Date) -> Result<(), NotComputable> {
    match state.coverage().last_event() {
        Some(last) if last > as_of => Err(NotComputable::StateNewerThanReport {
            last_event: last,
            as_of,
        }),
        _ => Ok(()),
    }
}

fn add(left: Dec, right: Dec) -> Result<Dec, NotComputable> {
    left.checked_add(right).map_err(numeric)
}

fn sub(left: Dec, right: Dec) -> Result<Dec, NotComputable> {
    left.checked_sub(right).map_err(numeric)
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
    use crate::valuation::{FxSource, FxTable, PriceQuality};
    use time::macros::date;

    #[test]
    fn an_owner_valuation_reaches_xirr_as_money_per_unit() {
        let account = AccountId::new_random();
        let instrument = InstrumentId::new_random();
        let custody = CustodyId::new_random();
        let contour = ContourDefinition::new(ContourId::new_random(), ContourVersion(1), [account]);
        let rules = RuleRegistry::with_defaults();
        let context = ProjectionContext {
            contour: &contour,
            rules: &rules,
            lot_rule: LotRuleVersion(1),
        };
        let quantity = qty(10);
        let opening = event_with(
            account,
            date!(2026 - 08 - 02),
            1,
            EventKind::OpeningPosition {
                instrument,
                quantity,
                cost_basis: None,
                assertions: Default::default(),
            },
            vec![Leg::security(account, custody, instrument, quantity)],
        );
        let valuation = event_with(
            account,
            date!(2026 - 08 - 03),
            2,
            EventKind::Valuation {
                instrument,
                price: Dec::new(rust_decimal::Decimal::from(98)),
                currency: CurrencyCode::Rub,
                quality: PriceQuality::OwnerEstimate,
            },
            vec![],
        );
        let state = project(&[opening, valuation], &context)
            .expect("owner-valuation projection")
            .snapshot()
            .state()
            .clone();
        let fx = FxTable::new(FxSource::OwnerSupplied);
        let ledger = ReconciliationLedger::default();
        let perimeter = PerimeterAssessment::empty(PerimeterPolicy::default());
        let request = ReturnsRequest {
            contour: &contour,
            as_of: date!(2026 - 08 - 03),
            report_currency: CurrencyCode::Rub,
            fx: &fx,
            solver_policy: SolverPolicy::returns_default(),
            coordinate: KnowledgeCoordinate::default(),
            ledger: &ledger,
            perimeter: &perimeter,
            market_prices: &[],
            bond_schedules: &std::collections::BTreeMap::new(),
            accrued_observations: &std::collections::BTreeMap::new(),
        };

        let values = account_values(&state, &request).expect("position value");

        assert_eq!(
            values[&account].positions,
            Dec::new(rust_decimal::Decimal::from(980))
        );
        assert_eq!(
            terminal_value(&state, &request).unwrap(),
            values[&account].total().unwrap(),
        );
    }

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
            .expect("projection")
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
            bond_schedules: &std::collections::BTreeMap::new(),
            accrued_observations: &std::collections::BTreeMap::new(),
            perimeter: &perimeter,
            market_prices: &[],
        };

        let values = account_values(snapshot.state(), &request).expect("account values");
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
