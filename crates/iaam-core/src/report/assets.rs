//! What the owner holds at a date, grouped by the class of cash he declared.
//!
//! The question no other report answers: how much is on deposit, how much on
//! savings accounts, how much on card accounts, how much in the wallet, how
//! much is invested, and what the whole is worth.
//!
//! **One fold.** The class totals and the per-account rows are the same
//! numbers: the rows are [`super::balances::BalancesReport`]'s own rows, and
//! the totals are folded from those rows here. Commit 54fc437 is the reason —
//! the architecture guard caught a cash fold done outside the core, and a total
//! reached by a second path can disagree with the rows it claims to summarise
//! without anything saying which of the two is wrong.
//!
//! **Two halves, and they are not the same kind of fact.** Cash is exact: it is
//! what the journal recorded, and nothing but the journal decides it. A
//! position is worth what a quote said on a date, and the date is part of the
//! figure. Adding the two into one number without saying which half moves makes
//! a market-dependent figure read like a bank figure, so both halves and the
//! oldest price behind them are stated **before** any total.
//!
//! **No conversion.** Every total is per currency. Converting would put a rate
//! inside the exact half and make the whole answer market-dependent, which is
//! precisely the confusion the split exists to prevent. A caller who wants one
//! number in one currency is asking for a valuation, which is what the returns
//! report computes.

use std::collections::BTreeMap;

use thiserror::Error;
use time::Date;

use crate::ids::{AccountId, InstrumentId};
use crate::money::{CalcMoney, CurrencyCode, Money, MoneyError, Quantity};
use crate::numeric::NumericError;
use crate::numeric::decimal::Dec;
use crate::projection::balances::PositionKey;
use crate::valuation::{InstrumentPrice, PriceBoard};

use super::balances::{AccountCash, BalancesReport, CashOpening};
use super::confidence::{Caveat, CaveatKind, CaveatSubject, ReportConfidence, ReportGoal};
use super::population::ReportPopulation;

/// What went wrong while folding a snapshot.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum AssetSnapshotError {
    #[error(transparent)]
    Money(#[from] MoneyError),
    #[error(transparent)]
    Numeric(#[from] NumericError),
}

/// One account inside the snapshot: what it holds, and the class it was
/// grouped under.
///
/// `cash_class` is a **code**, not a value of an enum this crate knows. The
/// class vocabulary lives in the storage adapter, where decision 0004 §3 put a
/// label no rule may read, and `iaam-core` depends on no workspace crate. That
/// is not an obstacle worked around: it is the prohibition made structural. The
/// core cannot branch on a class it cannot name, and the only thing it does
/// with the code is group by equality — the one consumer the decision allows.
///
/// `None` is «the owner has not said». It is its own group, never folded into a
/// default and never guessed at from a title.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AssetAccount {
    pub account: AccountId,
    pub cash_class: Option<String>,
    /// The account's cash, each figure carrying what is known about the state
    /// it accumulated from. A figure over an unasserted opening is a running
    /// sum and not a balance, and it is summed into the class total all the
    /// same — withholding it would make the total silently smaller, which is
    /// worse. The register says so instead.
    pub cash: Vec<AccountCash>,
    pub positions: Vec<(PositionKey, Quantity)>,
}

/// One class of cash and what the accounts declared to be it hold.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CashClassTotal {
    /// The class code, or `None` for the accounts whose class is not stated.
    pub cash_class: Option<String>,
    /// The accounts folded into this figure, so a total can be traced to the
    /// rows beneath it without re-deriving the grouping.
    pub accounts: Vec<AccountId>,
    /// One figure per currency, ascending by currency.
    pub totals: Vec<Money>,
    /// `Asserted` only when every figure summed here rests on an asserted
    /// opening. One running sum makes the class total a running sum too.
    pub opening: CashOpening,
}

/// The exact half: cash, as the journal recorded it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CashSide {
    /// One entry per class present in the population, ascending, with the
    /// unstated class first.
    pub classes: Vec<CashClassTotal>,
    /// Every class added up, per currency.
    pub totals: Vec<Money>,
    /// `Asserted` only when every figure in every class is.
    pub opening: CashOpening,
}

/// One instrument the owner holds, and what a quote said it was worth.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HoldingValue {
    pub instrument: InstrumentId,
    /// The quantity across every account and custody location in the scope. A
    /// position is keyed by all three, but «how much is invested» is a question
    /// about the instrument, and the per-account keys stay on the rows.
    pub quantity: Quantity,
    /// The quote used, with the date it was for. `None` — no quote in the
    /// journal covers this instrument at or before the report date.
    pub price: Option<InstrumentPrice>,
    /// `None` exactly when `price` is: an unpriced holding is **absent from the
    /// total, not valued at zero**. Zero is a figure the owner would add up;
    /// absence is a question, and the register names it.
    pub value: Option<CalcMoney>,
}

/// The market-dependent half: positions, at the prices the journal holds.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PositionsSide {
    /// One entry per instrument held, ascending by instrument.
    pub holdings: Vec<HoldingValue>,
    /// The earliest date any price behind these figures was for — the oldest
    /// link in the total, and the honest summary of «as of when». `None` when
    /// nothing was priced.
    ///
    /// The per-holding dates are on `holdings[].price`, because one summary
    /// date cannot say that one instrument is a day stale and another a year.
    pub oldest_price_date: Option<Date>,
    /// The priced holdings added up, per currency, in the currency each quote
    /// was made in. An unpriced holding is in no total.
    pub totals: Vec<CalcMoney>,
}

/// What the owner holds at a date: two halves, and the whole.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AssetSnapshot {
    pub as_of: Date,
    /// The exact half.
    pub cash: CashSide,
    /// The market-dependent half.
    pub positions: PositionsSide,
    /// Both halves added, per currency. **After** the halves, never instead of
    /// them: a reader who takes this figure alone cannot tell which part of it
    /// a market can move overnight.
    pub total: Vec<CalcMoney>,
    /// The rows the totals were folded from, in the balances answer's order.
    pub accounts: Vec<AssetAccount>,
    /// The accounts this answer covered, and the known accounts it did not. A
    /// total that silently omits an account is worse than no total.
    pub population: ReportPopulation,
}

impl AssetSnapshot {
    /// What would have to be true for this to be a complete statement of what
    /// the owner holds, and which of those things are not.
    ///
    /// Derived on demand from the fields above, exactly as
    /// [`BalancesReport::confidence`] is, and for the same reason: a stored
    /// register is a second copy that can fall behind the figures it
    /// summarises.
    #[must_use]
    pub fn confidence(&self) -> ReportConfidence {
        // The population first: an account left out of a total is the silence
        // no row can break.
        let mut caveats = self.population.caveats();
        for row in &self.accounts {
            for cash in &row.cash {
                if cash.opening == CashOpening::Unasserted {
                    caveats.push(Caveat::new(
                        CaveatKind::RunningCashSum,
                        CaveatSubject::AccountCurrency {
                            account: row.account,
                            currency: cash.money.currency(),
                        },
                    ));
                }
            }
        }
        for holding in &self.positions.holdings {
            if holding.value.is_none() {
                caveats.push(Caveat::new(
                    CaveatKind::HoldingNotValued,
                    CaveatSubject::Instrument(holding.instrument),
                ));
            }
        }
        ReportConfidence::new(ReportGoal::AssetSnapshot, caveats)
    }
}

/// Fold the balances answer into what the owner holds by class.
///
/// `classes` maps an account to the code the owner declared for it. An account
/// missing from the map has no declared class, which is a value: it lands in
/// the unstated group rather than in a default one.
///
/// `prices` is the journal's own price board — the same board
/// [`crate::projection::advance`] fills from `Valuation` events. It is not a
/// second market lookup: an instrument the journal never priced is reported as
/// unvalued rather than valued from a source this report picked on its own.
pub fn asset_snapshot(
    as_of: Date,
    report: &BalancesReport,
    classes: &BTreeMap<AccountId, String>,
    prices: &PriceBoard,
) -> Result<AssetSnapshot, AssetSnapshotError> {
    // One pass over the report's own rows produces the rows this answer
    // publishes; every total below is folded from that same vector, so no
    // figure here can disagree with a figure beside it.
    let accounts: Vec<AssetAccount> = report
        .accounts
        .iter()
        .map(|row| AssetAccount {
            account: row.account,
            cash_class: classes.get(&row.account).cloned(),
            cash: row.cash.clone(),
            positions: row.positions.clone(),
        })
        .collect();

    let cash = fold_cash(&accounts)?;
    let positions = fold_positions(&accounts, prices, as_of)?;
    let total = fold_total(&cash, &positions)?;

    Ok(AssetSnapshot {
        as_of,
        cash,
        positions,
        total,
        accounts,
        population: report.population.clone(),
    })
}

/// One class's accumulator while the fold runs.
struct ClassAccumulator {
    accounts: Vec<AccountId>,
    totals: BTreeMap<CurrencyCode, Money>,
    opening: CashOpening,
}

/// The exact half, grouped by the class the owner declared.
fn fold_cash(accounts: &[AssetAccount]) -> Result<CashSide, AssetSnapshotError> {
    // `None` sorts before `Some`, so the unstated group comes first: it is the
    // one the owner may still want to act on.
    let mut groups: BTreeMap<Option<String>, ClassAccumulator> = BTreeMap::new();
    for row in accounts {
        let entry = groups
            .entry(row.cash_class.clone())
            .or_insert_with(|| ClassAccumulator {
                accounts: Vec::new(),
                totals: BTreeMap::new(),
                opening: CashOpening::Asserted,
            });
        entry.accounts.push(row.account);
        for cash in &row.cash {
            add_money(&mut entry.totals, cash.money)?;
            if cash.opening == CashOpening::Unasserted {
                entry.opening = CashOpening::Unasserted;
            }
        }
    }

    let mut totals: BTreeMap<CurrencyCode, Money> = BTreeMap::new();
    let mut opening = CashOpening::Asserted;
    let mut classes = Vec::with_capacity(groups.len());
    for (cash_class, group) in groups {
        for money in group.totals.values() {
            add_money(&mut totals, *money)?;
        }
        if group.opening == CashOpening::Unasserted {
            opening = CashOpening::Unasserted;
        }
        classes.push(CashClassTotal {
            cash_class,
            accounts: group.accounts,
            totals: group.totals.into_values().collect(),
            opening: group.opening,
        });
    }

    Ok(CashSide {
        classes,
        totals: totals.into_values().collect(),
        opening,
    })
}

/// The market-dependent half, at the prices the journal holds.
fn fold_positions(
    accounts: &[AssetAccount],
    prices: &PriceBoard,
    as_of: Date,
) -> Result<PositionsSide, AssetSnapshotError> {
    let mut quantities: BTreeMap<InstrumentId, Quantity> = BTreeMap::new();
    for row in accounts {
        for (key, quantity) in &row.positions {
            let slot = quantities
                .entry(key.instrument)
                .or_insert_with(Quantity::zero);
            *slot = Quantity(slot.0.checked_add(quantity.0)?);
        }
    }

    let mut holdings = Vec::with_capacity(quantities.len());
    let mut totals: BTreeMap<CurrencyCode, CalcMoney> = BTreeMap::new();
    let mut oldest_price_date: Option<Date> = None;
    for (instrument, quantity) in quantities {
        let price = prices.price_at_or_before(instrument, as_of).copied();
        let value = match price {
            Some(price) => {
                let value = CalcMoney::new(price.price, price.currency).checked_mul(quantity.0)?;
                add_calc(&mut totals, value)?;
                oldest_price_date = Some(match oldest_price_date {
                    Some(known) if known <= price.as_of => known,
                    _ => price.as_of,
                });
                Some(value)
            }
            None => None,
        };
        holdings.push(HoldingValue {
            instrument,
            quantity,
            price,
            value,
        });
    }

    Ok(PositionsSide {
        holdings,
        oldest_price_date,
        totals: totals.into_values().collect(),
    })
}

/// Both halves, per currency.
///
/// Nothing is converted, so a currency present in only one half appears in the
/// whole as that half alone — which is the truth, and is why the halves stand
/// above this line rather than behind it.
fn fold_total(
    cash: &CashSide,
    positions: &PositionsSide,
) -> Result<Vec<CalcMoney>, AssetSnapshotError> {
    let mut total: BTreeMap<CurrencyCode, CalcMoney> = BTreeMap::new();
    for money in &cash.totals {
        add_calc(
            &mut total,
            CalcMoney::new(money.to_calc_dec(), money.currency()),
        )?;
    }
    for money in &positions.totals {
        add_calc(&mut total, *money)?;
    }
    Ok(total.into_values().collect())
}

fn add_money(
    totals: &mut BTreeMap<CurrencyCode, Money>,
    money: Money,
) -> Result<(), AssetSnapshotError> {
    let slot = totals
        .entry(money.currency())
        .or_insert_with(|| Money::zero(money.currency()));
    *slot = slot.try_add(money)?;
    Ok(())
}

fn add_calc(
    totals: &mut BTreeMap<CurrencyCode, CalcMoney>,
    money: CalcMoney,
) -> Result<(), AssetSnapshotError> {
    let slot = totals
        .entry(money.currency())
        .or_insert_with(|| CalcMoney::new(Dec::zero(), money.currency()));
    *slot = slot.checked_add(money)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::contour::{ContourId, ContourVersion};
    use crate::money::PostedMinor;
    use crate::report::balances::{AccountBalanceRow, PeriodReports};
    use crate::report::population::{AccountStanding, PopulationAccount};
    use crate::valuation::PriceQuality;
    use time::macros::date;
    use uuid::Uuid;

    const AS_OF: Date = date!(2026 - 01 - 31);

    fn account(index: u128) -> AccountId {
        AccountId(Uuid::from_u128(index))
    }

    fn instrument(index: u128) -> InstrumentId {
        InstrumentId(Uuid::from_u128(index + 500))
    }

    fn rub(minor: i64) -> Money {
        Money::new(PostedMinor::new(minor), CurrencyCode::Rub)
    }

    fn usd(minor: i64) -> Money {
        Money::new(PostedMinor::new(minor), CurrencyCode::Usd)
    }

    fn asserted(money: Money) -> AccountCash {
        AccountCash {
            money,
            opening: CashOpening::Asserted,
        }
    }

    fn row(account: AccountId, cash: Vec<AccountCash>) -> AccountBalanceRow {
        AccountBalanceRow {
            account,
            cash,
            reconciliation: Vec::new(),
            positions: Vec::new(),
            period_reports: PeriodReports::Calculated,
        }
    }

    fn report(accounts: Vec<AccountBalanceRow>) -> BalancesReport {
        let population = ReportPopulation {
            contour: ContourId(Uuid::from_u128(1)),
            version: ContourVersion(1),
            accounts: accounts
                .iter()
                .map(|row| PopulationAccount {
                    account: row.account,
                    title: format!("Account {}", row.account.inner()),
                    standing: AccountStanding::Covered,
                })
                .collect(),
        };
        BalancesReport {
            accounts,
            negative_cash: Vec::new(),
            population,
        }
    }

    fn classes(pairs: &[(AccountId, &str)]) -> BTreeMap<AccountId, String> {
        pairs
            .iter()
            .map(|(account, code)| (*account, (*code).to_owned()))
            .collect()
    }

    fn quantity(units: i64) -> Quantity {
        Quantity(Dec::new(units.into()))
    }

    fn position(account: AccountId, instrument: InstrumentId) -> PositionKey {
        PositionKey {
            account,
            custody: None,
            instrument,
        }
    }

    fn priced(board: &mut PriceBoard, instrument: InstrumentId, minor: i64, as_of: Date) {
        board.record(InstrumentPrice {
            instrument,
            price: Dec::new(minor.into()),
            currency: CurrencyCode::Rub,
            quality: PriceQuality::PreviousClose,
            as_of,
        });
    }

    fn total_in(totals: &[CalcMoney], currency: CurrencyCode) -> Dec {
        totals
            .iter()
            .find(|money| money.currency() == currency)
            .map_or_else(Dec::zero, CalcMoney::value)
    }

    /// The invariant the whole module exists for: the class totals and the
    /// per-account rows are one fold, so the totals cannot disagree with the
    /// rows they claim to summarise. Summing the published rows by hand must
    /// reproduce the published totals exactly — for every class and for the
    /// whole.
    #[test]
    fn every_total_is_the_sum_of_the_rows_it_summarises() {
        let deposit = account(10);
        let savings = account(11);
        let unstated = account(12);
        let report = report(vec![
            row(deposit, vec![asserted(rub(150_000))]),
            row(savings, vec![asserted(rub(40_000)), asserted(usd(2_500))]),
            row(unstated, vec![asserted(rub(700))]),
        ]);
        let snapshot = asset_snapshot(
            AS_OF,
            &report,
            &classes(&[(deposit, "deposit"), (savings, "savings")]),
            &PriceBoard::new(),
        )
        .expect("snapshot");

        for class in &snapshot.cash.classes {
            for total in &class.totals {
                let rows: Vec<Money> = snapshot
                    .accounts
                    .iter()
                    .filter(|row| class.accounts.contains(&row.account))
                    .flat_map(|row| row.cash.iter().map(|cash| cash.money))
                    .filter(|money| money.currency() == total.currency())
                    .collect();
                assert_eq!(
                    Money::sum(&rows, total.currency()).expect("sum"),
                    *total,
                    "class {:?} in {:?}",
                    class.cash_class,
                    total.currency()
                );
            }
        }

        for total in &snapshot.cash.totals {
            let rows: Vec<Money> = snapshot
                .accounts
                .iter()
                .flat_map(|row| row.cash.iter().map(|cash| cash.money))
                .filter(|money| money.currency() == total.currency())
                .collect();
            assert_eq!(Money::sum(&rows, total.currency()).expect("sum"), *total);
        }
    }

    /// An account whose class the owner has not stated is its own group. It is
    /// never folded into a default one, which would put his money under a
    /// heading he never chose.
    #[test]
    fn an_undeclared_class_is_its_own_group_and_never_a_default() {
        let stated = account(10);
        let unstated = account(11);
        let report = report(vec![
            row(stated, vec![asserted(rub(100_000))]),
            row(unstated, vec![asserted(rub(300))]),
        ]);
        let snapshot = asset_snapshot(
            AS_OF,
            &report,
            &classes(&[(stated, "deposit")]),
            &PriceBoard::new(),
        )
        .expect("snapshot");

        let groups: Vec<Option<&str>> = snapshot
            .cash
            .classes
            .iter()
            .map(|class| class.cash_class.as_deref())
            .collect();
        assert_eq!(groups, vec![None, Some("deposit")]);
        let not_stated = &snapshot.cash.classes[0];
        assert_eq!(not_stated.accounts, vec![unstated]);
        assert_eq!(not_stated.totals, vec![rub(300)]);
    }

    /// The two halves are stated apart and the whole is stated after them. The
    /// cash half must not move when a quote does, and the position half must
    /// carry the date its price was for.
    #[test]
    fn the_two_halves_are_separate_and_the_whole_is_their_sum() {
        let broker = account(10);
        let held = instrument(1);
        let mut rows = row(broker, vec![asserted(rub(5_000))]);
        rows.positions = vec![(position(broker, held), quantity(10))];
        let report = report(vec![rows]);

        let mut board = PriceBoard::new();
        priced(&mut board, held, 200, date!(2026 - 01 - 29));
        let snapshot = asset_snapshot(AS_OF, &report, &BTreeMap::new(), &board).expect("snapshot");

        assert_eq!(snapshot.cash.totals, vec![rub(5_000)]);
        assert_eq!(
            total_in(&snapshot.positions.totals, CurrencyCode::Rub),
            Dec::new(2_000.into())
        );
        assert_eq!(
            snapshot.positions.oldest_price_date,
            Some(date!(2026 - 01 - 29)),
            "the price date is stated with the half it moves"
        );
        // 5000 minor units of RUB is 50.00, plus 2000 of quoted value.
        assert_eq!(
            total_in(&snapshot.total, CurrencyCode::Rub),
            Dec::new(2_050.into())
        );
    }

    /// A holding no quote covers is absent from the total rather than valued at
    /// zero, and the register names it. Zero would be a number the owner could
    /// add up.
    #[test]
    fn an_unpriced_holding_is_absent_from_the_total_and_is_a_caveat() {
        let broker = account(10);
        let priced_one = instrument(1);
        let unpriced = instrument(2);
        let mut rows = row(broker, Vec::new());
        rows.positions = vec![
            (position(broker, priced_one), quantity(3)),
            (position(broker, unpriced), quantity(7)),
        ];
        let report = report(vec![rows]);

        let mut board = PriceBoard::new();
        priced(&mut board, priced_one, 100, date!(2026 - 01 - 30));
        let snapshot = asset_snapshot(AS_OF, &report, &BTreeMap::new(), &board).expect("snapshot");

        let missing = snapshot
            .positions
            .holdings
            .iter()
            .find(|holding| holding.instrument == unpriced)
            .expect("the unpriced holding is still listed");
        assert_eq!(missing.quantity, quantity(7));
        assert!(missing.price.is_none());
        assert!(missing.value.is_none(), "absent, never zero");
        assert_eq!(
            total_in(&snapshot.positions.totals, CurrencyCode::Rub),
            Dec::new(300.into()),
            "only the priced holding is in the total"
        );

        let confidence = snapshot.confidence();
        assert!(!confidence.complete());
        let caveat = confidence
            .caveats()
            .iter()
            .find(|caveat| caveat.kind() == CaveatKind::HoldingNotValued)
            .expect("a caveat names the unvalued holding");
        assert_eq!(caveat.subject(), CaveatSubject::Instrument(unpriced));
        assert_eq!(caveat.see(), "positions.holdings[].value");
    }

    /// A quantity that reaches the same instrument through two accounts is one
    /// holding: «how much is invested» is a question about the instrument, and
    /// two rows for it would let a reader add the same money twice.
    #[test]
    fn one_instrument_held_in_two_accounts_is_one_holding() {
        let first = account(10);
        let second = account(11);
        let held = instrument(1);
        let mut left = row(first, Vec::new());
        left.positions = vec![(position(first, held), quantity(4))];
        let mut right = row(second, Vec::new());
        right.positions = vec![(position(second, held), quantity(6))];
        let report = report(vec![left, right]);

        let mut board = PriceBoard::new();
        priced(&mut board, held, 10, date!(2026 - 01 - 20));
        let snapshot = asset_snapshot(AS_OF, &report, &BTreeMap::new(), &board).expect("snapshot");

        assert_eq!(snapshot.positions.holdings.len(), 1);
        assert_eq!(snapshot.positions.holdings[0].quantity, quantity(10));
    }

    /// The oldest price behind the total is the one reported: the total is only
    /// as fresh as its weakest link, and a summary naming the newest date would
    /// read as if the whole half were that fresh.
    #[test]
    fn the_price_date_reported_is_the_oldest_one_behind_the_total() {
        let broker = account(10);
        let fresh = instrument(1);
        let stale = instrument(2);
        let mut rows = row(broker, Vec::new());
        rows.positions = vec![
            (position(broker, fresh), quantity(1)),
            (position(broker, stale), quantity(1)),
        ];
        let report = report(vec![rows]);

        let mut board = PriceBoard::new();
        priced(&mut board, fresh, 100, date!(2026 - 01 - 30));
        priced(&mut board, stale, 100, date!(2025 - 06 - 02));
        let snapshot = asset_snapshot(AS_OF, &report, &BTreeMap::new(), &board).expect("snapshot");

        assert_eq!(
            snapshot.positions.oldest_price_date,
            Some(date!(2025 - 06 - 02))
        );
    }

    /// A figure accumulated from an unasserted start is a running sum, and a
    /// class total containing one is a running sum too. It is still summed:
    /// leaving it out would make the total quietly smaller, which is the worse
    /// of the two failures.
    #[test]
    fn a_running_sum_inside_a_class_makes_the_class_total_a_running_sum() {
        let savings = account(10);
        let report = report(vec![row(
            savings,
            vec![
                asserted(rub(1_000)),
                AccountCash {
                    money: usd(400),
                    opening: CashOpening::Unasserted,
                },
            ],
        )]);
        let snapshot = asset_snapshot(
            AS_OF,
            &report,
            &classes(&[(savings, "savings")]),
            &PriceBoard::new(),
        )
        .expect("snapshot");

        assert_eq!(snapshot.cash.classes[0].opening, CashOpening::Unasserted);
        assert_eq!(snapshot.cash.opening, CashOpening::Unasserted);
        assert!(snapshot.cash.totals.contains(&usd(400)));

        let confidence = snapshot.confidence();
        assert!(!confidence.complete());
        let caveat = confidence
            .caveats()
            .iter()
            .find(|caveat| caveat.kind() == CaveatKind::RunningCashSum)
            .expect("a running sum is a caveat");
        assert_eq!(
            caveat.subject(),
            CaveatSubject::AccountCurrency {
                account: savings,
                currency: CurrencyCode::Usd,
            }
        );
    }

    /// A total that silently omits an account is worse than no total, so the
    /// population's caveats travel with the snapshot and an incomplete
    /// population is never a complete answer.
    #[test]
    fn a_total_over_an_incomplete_population_never_reads_as_complete() {
        let covered = account(10);
        let mut report = report(vec![row(covered, vec![asserted(rub(1_000))])]);
        report.population.accounts.push(PopulationAccount {
            account: account(11),
            title: "Elsewhere".into(),
            standing: AccountStanding::OutsideUndecided,
        });
        let snapshot =
            asset_snapshot(AS_OF, &report, &BTreeMap::new(), &PriceBoard::new()).expect("snapshot");

        let confidence = snapshot.confidence();
        assert_eq!(confidence.goal(), ReportGoal::AssetSnapshot);
        assert!(!confidence.complete());
        assert!(
            confidence
                .caveats()
                .iter()
                .any(|caveat| caveat.kind() == CaveatKind::AccountInNoScope)
        );
    }

    /// The shape that may read as complete, so the assertions above are not
    /// passing because nothing ever does.
    #[test]
    fn a_whole_population_of_asserted_priced_holdings_is_complete() {
        let broker = account(10);
        let held = instrument(1);
        let mut rows = row(broker, vec![asserted(rub(1_000))]);
        rows.positions = vec![(position(broker, held), quantity(2))];
        let report = report(vec![rows]);

        let mut board = PriceBoard::new();
        priced(&mut board, held, 50, date!(2026 - 01 - 31));
        let snapshot = asset_snapshot(AS_OF, &report, &BTreeMap::new(), &board).expect("snapshot");

        let confidence = snapshot.confidence();
        assert!(confidence.complete(), "{:?}", confidence.caveats());
    }
}
