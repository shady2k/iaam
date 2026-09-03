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
//!
//! **One price, two reports.** The quote behind a holding is chosen by
//! [`crate::valuation::decide_price`] — the same call, over the same candidate
//! set, that the returns report makes for the same instrument on the same date.
//! This report once read the journal's board directly, which was honest but
//! narrow: an owner who had synced market data still saw his securities half
//! made of caveats. Reaching for the market store through a second selection of
//! its own would have been worse — two figures for one holding and nothing to
//! say which was wrong. The selection needs no report currency and no rate, so
//! taking it costs this report none of its exactness.

use std::collections::{BTreeMap, BTreeSet};

use thiserror::Error;
use time::Date;

use crate::bond::{BondSchedule, remaining_principal};
use crate::ids::{AccountId, InstrumentId};
use crate::money::{CalcMoney, CurrencyCode, Money, MoneyError, PerUnitAmount, Quantity};
use crate::numeric::NumericError;
use crate::numeric::decimal::Dec;
use crate::projection::balances::PositionKey;
use crate::returns::KnowledgeCoordinate;
use crate::rules::quotation::{QuotationRule, QuotationV1};
use crate::rules::valuation::SourcePriorityVersion;
use crate::valuation::{
    PriceBoard, PriceCandidate, PriceDecision, PriceInputs, PriceQuery, QuotationBasis,
    decide_price,
};

use super::balances::{AccountCash, BalancesReport, CashFigure, CashOpening};
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
    /// it accumulated from. A figure over an unasserted opening is movement
    /// from an unknown start and not a balance, and it is folded into the class
    /// total all the same — withholding it would make the total silently
    /// smaller, which is worse. It is folded into the movement part of that
    /// total, never into the balance part, so nothing is hidden and nothing is
    /// added to a figure it is not the same kind of thing as.
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
    /// One figure per currency, ascending by currency, each saying what kind of
    /// figure it is.
    ///
    /// **Per currency, and no flag beside the list.** A class held an
    /// `opening` of its own until this became [`CashFigure`], and that flag
    /// collapsed two things a reader needs apart: an opening assertion is per
    /// account *and currency*, so a class can be anchored in one currency and
    /// not in another, and one word for the class said `unasserted` for both.
    /// The distinction now rides on the figure it qualifies, which is the only
    /// place it cannot be dropped on the way to the number.
    ///
    /// Members of a class may disagree — some of its accounts anchored in this
    /// currency, some not — and then the figure is [`CashFigure::Mixed`] and
    /// carries the two parts rather than their sum. See that type for why the
    /// sum is not published.
    pub totals: Vec<CashFigure>,
}

/// The exact half: cash, as the journal recorded it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CashSide {
    /// One entry per class present in the population, ascending, with the
    /// unstated class first.
    pub classes: Vec<CashClassTotal>,
    /// Every class added up, per currency, on the same terms as a class total:
    /// a currency anchored on some accounts and not on others is `Mixed`, and
    /// its two parts are stated instead of their sum.
    pub totals: Vec<CashFigure>,
}

/// One instrument the owner holds, and what a quote said it was worth.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HoldingValue {
    pub instrument: InstrumentId,
    /// The quantity across every account and custody location in the scope. A
    /// position is keyed by all three, but «how much is invested» is a question
    /// about the instrument, and the per-account keys stay on the rows.
    pub quantity: Quantity,
    /// What the valuation policy decided for this instrument on this date, in
    /// full: the observation it chose and why, or the reason it chose none.
    ///
    /// The whole decision rather than a bare figure, and the same value the
    /// returns report publishes for the same instrument: a reader comparing the
    /// two reports is entitled to see that they rest on one observation, and a
    /// reader of one report is entitled to know that «not valued» meant «too
    /// old» rather than «never observed».
    pub price: PriceDecision,
    /// `None` whenever the decision yields no figure this report can turn into
    /// money: an unvalued holding is **absent from the total, not valued at
    /// zero**. Zero is a figure the owner would add up; absence is a question,
    /// and the register names it.
    pub value: Option<CalcMoney>,
}

/// The market-dependent half: positions, at the prices the policy selected.
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
    ///
    /// **A currency whose cash is not entirely balances has no entry here at
    /// all.** This is the one figure in the answer that cannot say what it is —
    /// it is a single number by definition, and the reader who stops at exactly
    /// one number stops at this one. Where part of the cash in a currency is
    /// movement from an unasserted start, no whole exists to state: adding a
    /// flow to a stock produces a figure that denotes nothing, and publishing
    /// it tagged would only move the trap here from the fields that now refuse
    /// it. So the currency is absent, exactly as an unvalued holding is absent
    /// from the position total rather than valued at zero — absence is a
    /// question, and [`AssetSnapshot::confidence`] names it. Both halves above
    /// still state everything they know, so nothing is withheld except the
    /// addition, and a reader who decides the addition means something can
    /// perform it.
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

/// Everything a holding needs a price from.
///
/// A struct rather than four arguments because the four travel together and
/// must describe **one** state of the world: a board folded from one journal, a
/// market slice read at one coordinate, and the coordinate itself. Passed
/// separately, a caller could hand this report a coordinate the returns report
/// did not use, and the two would disagree by construction.
#[derive(Debug, Clone, Copy)]
pub struct SnapshotPrices<'a> {
    /// The journal's own board — the same board [`crate::projection::advance`]
    /// fills from `Valuation` events, folded by `PriceBoard::observe`.
    pub board: &'a PriceBoard,
    /// Observations the shell read from the market store, for the instruments
    /// this report covers. Empty means the owner has synced nothing, which is a
    /// state this report describes rather than an error.
    pub market: &'a [PriceCandidate],
    /// Payment schedules at the knowledge coordinate, by instrument. Needed
    /// only by a quote made as a percentage of face value: without the
    /// schedule, such a quote is a number this report cannot turn into money,
    /// and the holding stays unvalued rather than being multiplied as if the
    /// percentage were roubles.
    pub schedules: &'a BTreeMap<InstrumentId, BondSchedule>,
    /// The coordinate the selection was made at. Shared with the returns
    /// report, which is what makes the two answers comparable.
    pub coordinate: KnowledgeCoordinate,
}

/// Fold the balances answer into what the owner holds by class.
///
/// `classes` maps an account to the code the owner declared for it. An account
/// missing from the map has no declared class, which is a value: it lands in
/// the unstated group rather than in a default one.
///
/// `prices` carries the two price channels and the coordinate they were read
/// at. The choice between them is not made here: it is
/// [`crate::valuation::decide_price`], the one the returns report makes.
pub fn asset_snapshot(
    as_of: Date,
    report: &BalancesReport,
    classes: &BTreeMap<AccountId, String>,
    prices: SnapshotPrices<'_>,
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
    let positions = fold_positions(&accounts, &prices, as_of)?;
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

/// One currency's cash while the fold runs: the two kinds kept apart.
///
/// They are never added into one accumulator, because the number that would
/// come out is the one this report refuses to publish. Kept apart, each part
/// stays a sum of like figures and the shape at the end says which parts there
/// were.
#[derive(Debug, Clone, Copy)]
struct CashParts {
    balance: Option<Money>,
    movement: Option<Money>,
}

impl CashParts {
    const EMPTY: Self = Self {
        balance: None,
        movement: None,
    };

    fn add(&mut self, figure: CashFigure) -> Result<(), AssetSnapshotError> {
        if let Some(balance) = figure.balance() {
            Self::accumulate(&mut self.balance, balance)?;
        }
        if let Some(movement) = figure.movement() {
            Self::accumulate(&mut self.movement, movement)?;
        }
        Ok(())
    }

    fn accumulate(slot: &mut Option<Money>, money: Money) -> Result<(), AssetSnapshotError> {
        *slot = Some(match *slot {
            Some(sum) => sum.try_add(money)?,
            None => money,
        });
        Ok(())
    }

    /// The figure these parts make. `None` only where nothing was added, which
    /// the fold never produces: an accumulator is created by its first figure.
    fn finish(self) -> Option<CashFigure> {
        match (self.balance, self.movement) {
            (Some(balance), Some(movement)) => Some(CashFigure::Mixed { balance, movement }),
            (Some(balance), None) => Some(CashFigure::Balance(balance)),
            (None, Some(movement)) => Some(CashFigure::Movement(movement)),
            (None, None) => None,
        }
    }
}

/// One class's accumulator while the fold runs.
struct ClassAccumulator {
    accounts: Vec<AccountId>,
    totals: BTreeMap<CurrencyCode, CashParts>,
}

/// The exact half, grouped by the class the owner declared.
///
/// The whole is accumulated from the same per-account figures as the classes,
/// in the same pass, rather than from the class totals afterwards. Both are one
/// fold of one set of rows, which is the property commit 54fc437 bought; and
/// re-adding class totals would mean taking a `Mixed` class apart again to add
/// its halves to the right places, which is the same arithmetic written twice.
fn fold_cash(accounts: &[AssetAccount]) -> Result<CashSide, AssetSnapshotError> {
    // `None` sorts before `Some`, so the unstated group comes first: it is the
    // one the owner may still want to act on.
    let mut groups: BTreeMap<Option<String>, ClassAccumulator> = BTreeMap::new();
    let mut whole: BTreeMap<CurrencyCode, CashParts> = BTreeMap::new();
    for row in accounts {
        let entry = groups
            .entry(row.cash_class.clone())
            .or_insert_with(|| ClassAccumulator {
                accounts: Vec::new(),
                totals: BTreeMap::new(),
            });
        entry.accounts.push(row.account);
        for cash in &row.cash {
            let figure = CashFigure::for_account(*cash);
            entry
                .totals
                .entry(figure.currency())
                .or_insert(CashParts::EMPTY)
                .add(figure)?;
            whole
                .entry(figure.currency())
                .or_insert(CashParts::EMPTY)
                .add(figure)?;
        }
    }

    let classes = groups
        .into_iter()
        .map(|(cash_class, group)| CashClassTotal {
            cash_class,
            accounts: group.accounts,
            totals: group
                .totals
                .into_values()
                .filter_map(CashParts::finish)
                .collect(),
        })
        .collect();

    Ok(CashSide {
        classes,
        totals: whole.into_values().filter_map(CashParts::finish).collect(),
    })
}

/// The figure a decision offers, in the unit and currency the source quoted it
/// in, with the date it was for.
///
/// `None` is «this decision yields no figure», and it covers three cases that
/// are one case here: nothing was selected, the old rule's determination lost
/// the observation it was made from, and the selected candidate's own evidence
/// contradicts the basis recorded for it. The returns report refuses the third
/// for the same reason: a price whose unit is disputed is not a price.
fn quoted(decision: &PriceDecision) -> Option<(Dec, CurrencyCode, QuotationBasis, Date)> {
    match decision {
        PriceDecision::Selected(selected) => {
            if selected.candidate.basis_evidence_contradicts {
                return None;
            }
            Some((
                selected.candidate.price,
                selected.candidate.currency,
                selected.candidate.basis,
                selected.candidate.trade_date,
            ))
        }
        // §10.3 again: a price stated in the journal is money per unit by
        // definition, which is why this arm may name the basis and the arm
        // above may not.
        PriceDecision::LegacyDerived { price, .. } => price.as_ref().map(|price| {
            (
                price.price,
                price.currency,
                QuotationBasis::MoneyPerUnit,
                price.as_of,
            )
        }),
        PriceDecision::Uncovered(_) => None,
    }
}

/// Money per security, from a quote whose unit may not be money.
///
/// The conversion is [`QuotationV1`], the rule the returns report uses, so a
/// bond quoted at a percentage of its remaining face value is worth the same in
/// both reports. A failure is `None` and not an error: the holding is left out
/// of the total with its caveat, which is this report's answer to «I do not
/// know» everywhere else.
fn money_per_unit(
    quote: (Dec, CurrencyCode, QuotationBasis, Date),
    schedules: &BTreeMap<InstrumentId, BondSchedule>,
    instrument: InstrumentId,
    as_of: Date,
) -> Option<(Dec, CurrencyCode)> {
    let (price, currency, basis, _) = quote;
    let remaining_face: Option<PerUnitAmount> = match basis {
        QuotationBasis::PercentOfRemainingFace => {
            Some(remaining_principal(schedules.get(&instrument)?, as_of).ok()?)
        }
        QuotationBasis::MoneyPerUnit | QuotationBasis::Unknown => None,
    };
    QuotationV1
        .money_per_unit(basis, price, currency, remaining_face)
        .ok()
}

/// The market-dependent half, at the prices the policy selected.
fn fold_positions(
    accounts: &[AssetAccount],
    prices: &SnapshotPrices<'_>,
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

    let inputs = PriceInputs {
        board: prices.board,
        market: prices.market,
        source_priority: SourcePriorityVersion(prices.coordinate.source_priority_version),
    };
    let mut holdings = Vec::with_capacity(quantities.len());
    let mut totals: BTreeMap<CurrencyCode, CalcMoney> = BTreeMap::new();
    let mut oldest_price_date: Option<Date> = None;
    for (instrument, quantity) in quantities {
        let price = decide_price(
            inputs,
            &PriceQuery {
                instrument,
                as_of,
                knowledge_as_of: prices.coordinate.knowledge_as_of,
            },
        );
        let quote = quoted(&price);
        let value = match quote
            .and_then(|quote| money_per_unit(quote, prices.schedules, instrument, as_of))
        {
            Some((per_unit, currency)) => {
                let value = CalcMoney::new(per_unit, currency).checked_mul(quantity.0)?;
                add_calc(&mut totals, value)?;
                // Only a holding that reached the total moves this date: an
                // observation the report could not turn into money is behind no
                // figure, and dating the total by it would age the total for
                // nothing.
                let trade_date = quote.expect("a value came from a quote").3;
                oldest_price_date = Some(match oldest_price_date {
                    Some(known) if known <= trade_date => known,
                    _ => trade_date,
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

/// Both halves, per currency, for the currencies that have a whole.
///
/// Nothing is converted, so a currency present in only one half appears in the
/// whole as that half alone — which is the truth, and is why the halves stand
/// above this line rather than behind it.
///
/// A currency whose cash is not entirely balances is left out, and left out
/// **completely**: its positions do not appear here either. The reason is not
/// that the position half is in doubt — it is not — but that this list is a
/// list of wholes, and a currency's whole is exactly what is unknown while part
/// of its cash is movement from an unasserted start. A row carrying only the
/// position half would be a smaller wrong answer to the same question, and
/// [`PositionsSide::totals`] already states that half where it belongs.
fn fold_total(
    cash: &CashSide,
    positions: &PositionsSide,
) -> Result<Vec<CalcMoney>, AssetSnapshotError> {
    let mut total: BTreeMap<CurrencyCode, CalcMoney> = BTreeMap::new();
    let mut without_a_whole: BTreeSet<CurrencyCode> = BTreeSet::new();
    for figure in &cash.totals {
        match figure {
            CashFigure::Balance(money) => add_calc(
                &mut total,
                CalcMoney::new(money.to_calc_dec(), money.currency()),
            )?,
            CashFigure::Movement(_) | CashFigure::Mixed { .. } => {
                without_a_whole.insert(figure.currency());
            }
        }
    }
    for money in &positions.totals {
        if !without_a_whole.contains(&money.currency()) {
            add_calc(&mut total, *money)?;
        }
    }
    Ok(total.into_values().collect())
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
    use crate::valuation::{
        InstrumentPrice, PriceKind, PriceOrigin, PriceQuality, SourceExecutability,
        UncoveredReason, Venue,
    };
    use time::macros::{date, datetime};
    use uuid::Uuid;

    const AS_OF: Date = date!(2026 - 01 - 31);

    /// No bond schedules. A `static` because [`SnapshotPrices`] borrows the map
    /// and a helper cannot lend out a local.
    static NO_SCHEDULES: BTreeMap<InstrumentId, BondSchedule> = BTreeMap::new();

    /// The inputs as they stand for an owner who has synced no market data:
    /// the journal's board and nothing else. What this report saw before the
    /// market channel reached it, and still sees when that channel is empty.
    fn journal_only(board: &PriceBoard) -> SnapshotPrices<'_> {
        SnapshotPrices {
            board,
            market: &[],
            schedules: &NO_SCHEDULES,
            coordinate: KnowledgeCoordinate::default(),
        }
    }

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

    fn unasserted(money: Money) -> AccountCash {
        AccountCash {
            money,
            opening: CashOpening::Unasserted,
        }
    }

    /// The figure a total of asserted rows folds to.
    const fn balance(money: Money) -> CashFigure {
        CashFigure::Balance(money)
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
            journal_only(&PriceBoard::new()),
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
                    balance(Money::sum(&rows, total.currency()).expect("sum")),
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
            assert_eq!(
                balance(Money::sum(&rows, total.currency()).expect("sum")),
                *total
            );
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
            journal_only(&PriceBoard::new()),
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
        assert_eq!(not_stated.totals, vec![balance(rub(300))]);
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
        let snapshot = asset_snapshot(AS_OF, &report, &BTreeMap::new(), journal_only(&board))
            .expect("snapshot");

        assert_eq!(snapshot.cash.totals, vec![balance(rub(5_000))]);
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
        let snapshot = asset_snapshot(AS_OF, &report, &BTreeMap::new(), journal_only(&board))
            .expect("snapshot");

        let missing = snapshot
            .positions
            .holdings
            .iter()
            .find(|holding| holding.instrument == unpriced)
            .expect("the unpriced holding is still listed");
        assert_eq!(missing.quantity, quantity(7));
        assert_eq!(
            missing.price,
            PriceDecision::Uncovered(UncoveredReason::NoObservation),
            "the report says why, not merely that it could not"
        );
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
        let snapshot = asset_snapshot(AS_OF, &report, &BTreeMap::new(), journal_only(&board))
            .expect("snapshot");

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
        priced(&mut board, stale, 100, date!(2026 - 01 - 05));
        let snapshot = asset_snapshot(AS_OF, &report, &BTreeMap::new(), journal_only(&board))
            .expect("snapshot");

        assert_eq!(
            snapshot.positions.oldest_price_date,
            Some(date!(2026 - 01 - 05))
        );
    }

    /// A class anchored in one currency and not in another says so per
    /// currency. The single `opening` word this replaced could not: it said
    /// `unasserted` for the whole class, which understated the currency that
    /// was anchored.
    ///
    /// The unanchored figure is still folded in. Leaving it out would make the
    /// total quietly smaller, which is the worse of the two failures.
    #[test]
    fn a_class_says_per_currency_which_of_its_figures_are_anchored() {
        let savings = account(10);
        let report = report(vec![row(
            savings,
            vec![asserted(rub(1_000)), unasserted(usd(400))],
        )]);
        let snapshot = asset_snapshot(
            AS_OF,
            &report,
            &classes(&[(savings, "savings")]),
            journal_only(&PriceBoard::new()),
        )
        .expect("snapshot");

        assert_eq!(
            snapshot.cash.classes[0].totals,
            vec![
                CashFigure::Balance(rub(1_000)),
                CashFigure::Movement(usd(400))
            ]
        );
        assert_eq!(snapshot.cash.totals, snapshot.cash.classes[0].totals);

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

    /// The case the class total exists to get right: two accounts in one class
    /// and one currency, one anchored and one not.
    ///
    /// Their sum is neither a balance nor a movement — a stock added to a flow
    /// — so no sum is published. Both parts are, each a sum of like figures,
    /// and the reader who wants one number performs the addition knowing what
    /// he is adding.
    #[test]
    fn a_class_whose_members_disagree_publishes_both_parts_and_no_sum() {
        let anchored = account(10);
        let adrift = account(11);
        let report = report(vec![
            row(anchored, vec![asserted(rub(1_000))]),
            row(adrift, vec![unasserted(rub(400))]),
        ]);
        let snapshot = asset_snapshot(
            AS_OF,
            &report,
            &classes(&[(anchored, "savings"), (adrift, "savings")]),
            journal_only(&PriceBoard::new()),
        )
        .expect("snapshot");

        let mixed = CashFigure::Mixed {
            balance: rub(1_000),
            movement: rub(400),
        };
        assert_eq!(snapshot.cash.classes[0].totals, vec![mixed]);
        assert_eq!(snapshot.cash.totals, vec![mixed]);
        // Nothing anywhere in the answer states 1400: that figure is what the
        // shape refuses.
        assert_eq!(mixed.balance(), Some(rub(1_000)));
        assert_eq!(mixed.movement(), Some(rub(400)));
    }

    /// A currency whose cash is not entirely balances has no whole, so it has
    /// no entry in `total` — not even for the position half, which is exact.
    /// The list is a list of wholes, and half of one is a smaller wrong answer
    /// to the same question.
    #[test]
    fn a_currency_without_an_anchor_has_no_entry_in_the_whole() {
        let adrift = account(10);
        let held = instrument(1);
        let mut rows = row(adrift, vec![unasserted(rub(5_000))]);
        rows.positions = vec![(position(adrift, held), quantity(10))];
        let report = report(vec![rows]);

        let mut board = PriceBoard::new();
        priced(&mut board, held, 200, date!(2026 - 01 - 29));
        let snapshot = asset_snapshot(AS_OF, &report, &BTreeMap::new(), journal_only(&board))
            .expect("snapshot");

        assert_eq!(snapshot.cash.totals, vec![CashFigure::Movement(rub(5_000))]);
        assert_eq!(
            total_in(&snapshot.positions.totals, CurrencyCode::Rub),
            Dec::new(2_000.into()),
            "the position half still states what it knows"
        );
        assert!(
            snapshot.total.is_empty(),
            "no whole exists while the cash is movement from an unknown start: {:?}",
            snapshot.total
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
        let snapshot = asset_snapshot(
            AS_OF,
            &report,
            &BTreeMap::new(),
            journal_only(&PriceBoard::new()),
        )
        .expect("snapshot");

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
        let snapshot = asset_snapshot(AS_OF, &report, &BTreeMap::new(), journal_only(&board))
            .expect("snapshot");

        let confidence = snapshot.confidence();
        assert!(confidence.complete(), "{:?}", confidence.caveats());
    }

    fn market_quote(instrument: InstrumentId, minor: i64, trade_date: Date) -> PriceCandidate {
        PriceCandidate {
            instrument,
            price: Dec::new(minor.into()),
            currency: CurrencyCode::Rub,
            basis: QuotationBasis::MoneyPerUnit,
            basis_evidence: "market:board".to_owned(),
            basis_evidence_contradicts: false,
            trade_date,
            observed_at: Some(datetime!(2026 - 01 - 31 18:00 UTC)),
            origin: PriceOrigin::Market {
                venue: Venue {
                    board: "MAIN".to_owned(),
                    session: 1,
                },
                kind: PriceKind::LegalClose,
            },
            executability: SourceExecutability::Executable,
        }
    }

    /// The reason this report was given a market channel: an owner who never
    /// entered a valuation event still owns something, and a synced quote says
    /// what it was worth. Before this, his securities half was caveats.
    #[test]
    fn a_market_quote_values_a_holding_the_journal_never_priced() {
        let broker = account(10);
        let held = instrument(1);
        let mut rows = row(broker, Vec::new());
        rows.positions = vec![(position(broker, held), quantity(4))];
        let report = report(vec![rows]);

        let board = PriceBoard::new();
        let market = [market_quote(held, 250, date!(2026 - 01 - 30))];
        let snapshot = asset_snapshot(
            AS_OF,
            &report,
            &BTreeMap::new(),
            SnapshotPrices {
                board: &board,
                market: &market,
                schedules: &NO_SCHEDULES,
                coordinate: KnowledgeCoordinate {
                    knowledge_as_of: datetime!(2026 - 02 - 01 12:00 UTC),
                    source_priority_version: 1,
                    valuation_policy_version: 1,
                },
            },
        )
        .expect("snapshot");

        let holding = &snapshot.positions.holdings[0];
        let selected = holding.price.selected().expect("a market quote was chosen");
        assert!(matches!(
            selected.candidate.origin,
            PriceOrigin::Market { .. }
        ));
        assert_eq!(
            total_in(&snapshot.positions.totals, CurrencyCode::Rub),
            Dec::new(1_000.into())
        );
        assert!(snapshot.confidence().complete());
    }

    /// Broadening the price source must not turn «I do not know» into a number.
    /// A quote the valuation policy refuses as too old leaves the holding out
    /// of the total — where the returns report has always left it — and the
    /// refusal keeps its reason.
    #[test]
    fn a_quote_past_the_maximum_age_values_nothing_and_says_why() {
        let broker = account(10);
        let held = instrument(1);
        let mut rows = row(broker, Vec::new());
        rows.positions = vec![(position(broker, held), quantity(4))];
        let report = report(vec![rows]);

        let mut board = PriceBoard::new();
        priced(&mut board, held, 250, date!(2025 - 06 - 02));
        let snapshot = asset_snapshot(AS_OF, &report, &BTreeMap::new(), journal_only(&board))
            .expect("snapshot");

        let holding = &snapshot.positions.holdings[0];
        assert_eq!(
            holding.price,
            PriceDecision::Uncovered(UncoveredReason::TooOld)
        );
        assert!(holding.value.is_none(), "absent, never zero");
        assert!(snapshot.positions.totals.is_empty());
        assert_eq!(snapshot.positions.oldest_price_date, None);
        assert!(
            snapshot
                .confidence()
                .caveats()
                .iter()
                .any(|caveat| caveat.kind() == CaveatKind::HoldingNotValued)
        );
    }

    /// A percentage of face value is not roubles. Without the schedule that
    /// says what the face value now is, the number cannot become money, and
    /// multiplying it by the quantity would publish a figure off by orders of
    /// magnitude with nothing marking it.
    #[test]
    fn a_percent_of_face_quote_without_a_schedule_values_nothing() {
        let broker = account(10);
        let held = instrument(1);
        let mut rows = row(broker, Vec::new());
        rows.positions = vec![(position(broker, held), quantity(4))];
        let report = report(vec![rows]);

        let board = PriceBoard::new();
        let mut quote = market_quote(held, 98, date!(2026 - 01 - 30));
        quote.basis = QuotationBasis::PercentOfRemainingFace;
        let market = [quote];
        let snapshot = asset_snapshot(
            AS_OF,
            &report,
            &BTreeMap::new(),
            SnapshotPrices {
                board: &board,
                market: &market,
                schedules: &NO_SCHEDULES,
                coordinate: KnowledgeCoordinate {
                    knowledge_as_of: datetime!(2026 - 02 - 01 12:00 UTC),
                    source_priority_version: 1,
                    valuation_policy_version: 1,
                },
            },
        )
        .expect("snapshot");

        let holding = &snapshot.positions.holdings[0];
        assert!(
            holding.price.selected().is_some(),
            "the quote was selected; it is the unit that is missing"
        );
        assert!(holding.value.is_none(), "absent, never 98 times four");
        assert!(snapshot.positions.totals.is_empty());
        assert!(
            snapshot
                .confidence()
                .caveats()
                .iter()
                .any(|caveat| caveat.kind() == CaveatKind::HoldingNotValued)
        );
    }
}
