//! Те же величины, посчитанные из журнала (§10.3).
//!
//! Это **вторая сторона** сверки. Первая — то, что сказал источник
//! ([`super::claim`]). Стороны обязаны считаться независимо: общий
//! помощник между ними превратил бы проверку в тавтологию, и
//! компенсирующая ошибка разбора перестала бы ловиться — ровно то,
//! ради чего §10.3 вводит три уровня достоверности, а не два.
//!
//! Остатки берутся у [`Balances`] — уже проверенной проекции. Это не
//! нарушает независимость: `Balances` считает по журналу, а не по
//! контрольной секции документа, и общего кода с разбором отчёта у неё
//! нет.

use std::collections::BTreeMap;

use thiserror::Error;

use super::claim::{AssertionPeriod, BalancePoint};
use crate::event::Event;
use crate::event::kind::EventKind;
use crate::event::leg::LegKind;
use crate::ids::{AccountId, CustodyId, EventId, InstrumentId};
use crate::money::{CurrencyCode, PostedMinor, Quantity};
use crate::numeric::NumericError;
use crate::projection::balances::{BalanceError, Balances, PositionKey};

/// Обороты по счёту за интервал.
///
/// Обе стороны — **модули**. `debit` — приход, `credit` — расход;
/// соответствие колонкам конкретного отчёта устанавливает парсер,
/// а не эта структура.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Turnover {
    pub debit: PostedMinor,
    pub credit: PostedMinor,
}

impl Default for Turnover {
    /// Нулевой оборот пишется здесь руками, а не выводится из
    /// `Default` для [`PostedMinor`]: денежный тип намеренно не имеет
    /// умолчания, потому что нулевая заглушка вместо неизвестной суммы
    /// — это ровно то, что запрещает §4.9. У оборота ноль осмыслен:
    /// он означает «движений не было», и накопитель начинает с него
    /// только там, где счёт уже признан существующим.
    fn default() -> Self {
        Self {
            debit: PostedMinor::new(0),
            credit: PostedMinor::new(0),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum ObserveError {
    #[error("событие {event:?} не имеет ни одной даты и не попадает ни в один период")]
    EventWithoutDate { event: EventId },
    #[error("переполнение при подсчёте величины {field}")]
    Overflow { field: &'static str },
    #[error(transparent)]
    Balance(#[from] BalanceError),
    #[error(transparent)]
    Numeric(#[from] NumericError),
}

/// Наблюдаемые величины за интервал.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ObservedTotals {
    cash_opening: BTreeMap<CurrencyCode, PostedMinor>,
    cash_closing: BTreeMap<CurrencyCode, PostedMinor>,
    positions_opening: BTreeMap<(InstrumentId, CustodyId), Quantity>,
    positions_closing: BTreeMap<(InstrumentId, CustodyId), Quantity>,
    turnover: BTreeMap<CurrencyCode, Turnover>,
    fees: BTreeMap<CurrencyCode, PostedMinor>,
    income: BTreeMap<CurrencyCode, PostedMinor>,
    tax_withheld: BTreeMap<CurrencyCode, PostedMinor>,
    tax_facts_recorded: bool,
    events_seen: u64,
}

impl ObservedTotals {
    #[must_use]
    pub fn cash_at(&self, at: BalancePoint, currency: CurrencyCode) -> Option<PostedMinor> {
        match at {
            BalancePoint::Opening => self.cash_opening.get(&currency).copied(),
            BalancePoint::Closing => self.cash_closing.get(&currency).copied(),
        }
    }

    #[must_use]
    pub fn position_at(
        &self,
        at: BalancePoint,
        instrument: InstrumentId,
        custody: CustodyId,
    ) -> Option<Quantity> {
        match at {
            BalancePoint::Opening => self.positions_opening.get(&(instrument, custody)).copied(),
            BalancePoint::Closing => self.positions_closing.get(&(instrument, custody)).copied(),
        }
    }

    #[must_use]
    pub fn turnover(&self, currency: CurrencyCode) -> Option<Turnover> {
        self.turnover.get(&currency).copied()
    }

    #[must_use]
    pub fn fees(&self, currency: CurrencyCode) -> Option<PostedMinor> {
        self.fees.get(&currency).copied()
    }

    #[must_use]
    pub fn income(&self, currency: CurrencyCode) -> Option<PostedMinor> {
        self.income.get(&currency).copied()
    }

    #[must_use]
    pub fn tax_withheld(&self, currency: CurrencyCode) -> Option<PostedMinor> {
        self.tax_withheld.get(&currency).copied()
    }

    /// Записан ли в журнале хоть один факт удержанного налога.
    ///
    /// Ложь означает «сравнивать не с чем», а не «налог равен нулю».
    /// Налоговые факты появляются в E5; до тех пор утверждение отчёта
    /// об удержанном налоге не является расхождением.
    #[must_use]
    pub const fn tax_facts_recorded(&self) -> bool {
        self.tax_facts_recorded
    }

    /// Сколько событий счёта видел журнал за интервал и до него.
    /// Ноль означает, что подтверждать нечего: истории нет.
    #[must_use]
    pub const fn events_seen(&self) -> u64 {
        self.events_seen
    }
}

/// Подсчёт наблюдаемых величин за интервал.
///
/// Логика вынесена из конструктора с именем `new` намеренно:
/// `cargo-mutants` молча пропускает функции с этим именем (§15.7).
pub fn observe(
    events: &[Event],
    account: AccountId,
    period: AssertionPeriod,
) -> Result<ObservedTotals, ObserveError> {
    let mut opening = Balances::new();
    let mut closing = Balances::new();
    let mut totals = ObservedTotals::default();

    for event in events {
        let date = event
            .dates
            .effective_date()
            .ok_or(ObserveError::EventWithoutDate { event: event.id })?;

        let touches_us = event.legs.iter().any(|leg| leg.account == account);
        if date < period.from {
            opening.apply(event)?;
            closing.apply(event)?;
            if touches_us {
                totals.events_seen += 1;
            }
        } else if period.contains(date) {
            closing.apply(event)?;
            if touches_us {
                totals.events_seen += 1;
                accumulate(&mut totals, event, account)?;
            }
        }
        // События позже конца интервала не применяются ни к чему:
        // остаток на конец марта не знает про апрель.
    }

    snapshot_cash(&opening, account, &mut totals.cash_opening);
    snapshot_cash(&closing, account, &mut totals.cash_closing);
    snapshot_positions(&opening, account, &mut totals.positions_opening);
    snapshot_positions(&closing, account, &mut totals.positions_closing);
    Ok(totals)
}

fn snapshot_cash(
    balances: &Balances,
    account: AccountId,
    into: &mut BTreeMap<CurrencyCode, PostedMinor>,
) {
    for (owner, money) in balances.iter_cash() {
        if owner == account {
            into.insert(money.currency(), money.amount());
        }
    }
}

fn snapshot_positions(
    balances: &Balances,
    account: AccountId,
    into: &mut BTreeMap<(InstrumentId, CustodyId), Quantity>,
) {
    for (key, quantity) in balances.iter_positions() {
        let PositionKey {
            account: owner,
            custody,
            instrument,
        } = key;
        if *owner != account {
            continue;
        }
        // Утверждение отчёта всегда называет депозитарий, поэтому
        // позиция, записанная без него, сверке не подлежит и в срез
        // не попадает: сравнить её было бы не с чем.
        if let Some(custody) = custody {
            into.insert((*instrument, *custody), quantity);
        }
    }
}

/// Накопление величин интервала по ногам **нашего** счёта.
fn accumulate(
    totals: &mut ObservedTotals,
    event: &Event,
    account: AccountId,
) -> Result<(), ObserveError> {
    let is_income = matches!(event.kind, EventKind::Income { .. });
    for leg in &event.legs {
        if leg.account != account {
            continue;
        }
        let Some(money) = leg.cash_effect() else {
            continue;
        };
        let currency = money.currency();
        let raw = money.amount().raw();

        let turnover = totals.turnover.entry(currency).or_default();
        if raw >= 0 {
            turnover.debit = turnover
                .debit
                .checked_add(PostedMinor::new(raw))
                .ok_or(ObserveError::Overflow { field: "debit" })?;
        } else {
            let magnitude = raw
                .checked_neg()
                .ok_or(ObserveError::Overflow { field: "credit" })?;
            turnover.credit = turnover
                .credit
                .checked_add(PostedMinor::new(magnitude))
                .ok_or(ObserveError::Overflow { field: "credit" })?;
        }

        match leg.kind {
            LegKind::Fee => add_magnitude(&mut totals.fees, currency, raw, "fees")?,
            LegKind::Tax => {
                totals.tax_facts_recorded = true;
                add_magnitude(&mut totals.tax_withheld, currency, raw, "tax_withheld")?;
            }
            LegKind::Cash => {
                if is_income {
                    add_magnitude(&mut totals.income, currency, raw, "income")?;
                }
            }
            LegKind::SecurityQuantity | LegKind::Principal => {}
        }
    }
    Ok(())
}

/// Прибавление модуля величины: контрольные суммы отчёта — модули,
/// знак в них несёт название колонки, а не число.
fn add_magnitude(
    into: &mut BTreeMap<CurrencyCode, PostedMinor>,
    currency: CurrencyCode,
    raw: i64,
    field: &'static str,
) -> Result<(), ObserveError> {
    let magnitude = raw.checked_abs().ok_or(ObserveError::Overflow { field })?;
    let slot = into.entry(currency).or_insert_with(|| PostedMinor::new(0));
    *slot = slot
        .checked_add(PostedMinor::new(magnitude))
        .ok_or(ObserveError::Overflow { field })?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::event::kind::{FeeOrigin, TradeSide};
    use crate::event::leg::Leg;
    use crate::event::test_support::event_with;
    use crate::money::Money;
    use crate::numeric::decimal::Dec;
    use rust_decimal::Decimal;
    use time::macros::date;

    fn rub(minor: i64) -> Money {
        Money::new(PostedMinor::new(minor), CurrencyCode::Rub)
    }

    fn march() -> AssertionPeriod {
        AssertionPeriod::between(date!(2026 - 03 - 01), date!(2026 - 03 - 31)).unwrap()
    }

    fn qty(units: i64) -> Quantity {
        Quantity(Dec::new(Decimal::from(units)))
    }

    fn per_unit(text: &str) -> crate::money::PerUnitAmount {
        crate::money::PerUnitAmount::new(
            Dec::new(Decimal::from_str_exact(text).unwrap()),
            CurrencyCode::Rub,
        )
    }

    /// Позиция по облигации на начало марта плюс корпоративное действие.
    ///
    /// Открывающая позиция нужна, чтобы у среза было что показывать:
    /// амортизация количества не двигает, и без неё сравнивать нечего.
    fn bond_events(
        account: AccountId,
        instrument: InstrumentId,
        custody: CustodyId,
        action: crate::event::corporate_action::CorporateAction,
        action_legs: Vec<Leg>,
    ) -> Vec<Event> {
        vec![
            event_with(
                account,
                date!(2026 - 02 - 10),
                1,
                EventKind::OpeningPosition {
                    instrument,
                    quantity: qty(10),
                    cost_basis: Some(rub(1_000_000)),
                    assertions: crate::event::kind::OpeningAssertions::default(),
                },
                vec![Leg::security(account, custody, instrument, qty(10))],
            ),
            event_with(
                account,
                date!(2026 - 03 - 15),
                1,
                EventKind::CorporateAction { action },
                action_legs,
            ),
        ]
    }

    #[test]
    fn amortisation_moves_cash_but_not_the_position_count() {
        // §6.5: выплата есть, выбытия нет. Изменение количества здесь
        // означало бы расхождение с брокерским отчётом на ровном месте.
        let account = AccountId::new_random();
        let instrument = InstrumentId::new_random();
        let custody = CustodyId::new_random();
        let events = bond_events(
            account,
            instrument,
            custody,
            crate::event::corporate_action::CorporateAction::PartialRedemption {
                instrument,
                custody,
                quantity: qty(10),
                principal_returned_per_unit: per_unit("200"),
                compensation: rub(200_000),
                effective_date: date!(2026 - 03 - 15),
                record_date: None,
                grounds: None,
                basis_allocation: crate::event::allocation::BasisAllocation::default(),
            },
            vec![Leg::principal(account, instrument, rub(200_000))],
        );

        let observed = observe(&events, account, march()).unwrap();
        assert_eq!(
            observed.turnover(CurrencyCode::Rub).map(|t| t.debit),
            Some(PostedMinor::new(200_000)),
            "нога Principal обязана попасть в оборот: она уже денежная"
        );
        assert_eq!(
            observed.position_at(BalancePoint::Closing, instrument, custody),
            Some(qty(10)),
            "амортизация не выводит бумагу из позиции"
        );
    }

    #[test]
    fn amortisation_is_not_counted_as_income() {
        // Возврат собственного капитала доходом не является (§6.5).
        let account = AccountId::new_random();
        let instrument = InstrumentId::new_random();
        let custody = CustodyId::new_random();
        let events = bond_events(
            account,
            instrument,
            custody,
            crate::event::corporate_action::CorporateAction::PartialRedemption {
                instrument,
                custody,
                quantity: qty(10),
                principal_returned_per_unit: per_unit("200"),
                compensation: rub(200_000),
                effective_date: date!(2026 - 03 - 15),
                record_date: None,
                grounds: None,
                basis_allocation: crate::event::allocation::BasisAllocation::default(),
            },
            vec![Leg::principal(account, instrument, rub(200_000))],
        );

        let observed = observe(&events, account, march()).unwrap();
        assert_eq!(observed.income(CurrencyCode::Rub), None);
    }

    #[test]
    fn a_redemption_moves_both_the_cash_and_the_position() {
        let account = AccountId::new_random();
        let instrument = InstrumentId::new_random();
        let custody = CustodyId::new_random();
        let events = bond_events(
            account,
            instrument,
            custody,
            crate::event::corporate_action::CorporateAction::Redemption {
                instrument,
                custody,
                quantity: qty(10),
                principal_returned_per_unit: per_unit("1000"),
                compensation: rub(1_000_000),
                effective_date: date!(2026 - 03 - 15),
                record_date: None,
                grounds: None,
            },
            vec![
                Leg::principal(account, instrument, rub(1_000_000)),
                Leg::security(account, custody, instrument, qty(-10)),
            ],
        );

        let observed = observe(&events, account, march()).unwrap();
        assert_eq!(
            observed.turnover(CurrencyCode::Rub).map(|t| t.debit),
            Some(PostedMinor::new(1_000_000))
        );
        assert_eq!(
            observed.position_at(BalancePoint::Closing, instrument, custody),
            Some(qty(0)),
            "погашенная бумага не остаётся в позиции"
        );
    }

    #[test]
    fn opening_excludes_the_period_and_closing_includes_it() {
        // Остаток на начало марта — это состояние до первого мартовского
        // события. Включить март в «начало» значит сверять отчёт с самим
        // собой: обе стороны съедут одинаково, и расхождение исчезнет.
        let account = AccountId::new_random();
        let events = vec![
            event_with(
                account,
                date!(2026 - 02 - 20),
                1,
                EventKind::CashIn {
                    amount: rub(100_000),
                },
                vec![Leg::cash(account, rub(100_000))],
            ),
            event_with(
                account,
                date!(2026 - 03 - 10),
                1,
                EventKind::CashIn {
                    amount: rub(50_000),
                },
                vec![Leg::cash(account, rub(50_000))],
            ),
            event_with(
                account,
                date!(2026 - 04 - 05),
                1,
                EventKind::CashIn { amount: rub(7) },
                vec![Leg::cash(account, rub(7))],
            ),
        ];

        let observed = observe(&events, account, march()).unwrap();
        assert_eq!(
            observed.cash_at(BalancePoint::Opening, CurrencyCode::Rub),
            Some(PostedMinor::new(100_000))
        );
        assert_eq!(
            observed.cash_at(BalancePoint::Closing, CurrencyCode::Rub),
            Some(PostedMinor::new(150_000)),
            "апрельское событие не имеет права попасть в остаток на конец марта"
        );
    }

    #[test]
    fn turnover_counts_every_cash_leg_including_fees() {
        // Оборот по счёту — это всё движение денег, а не только ноги
        // типа Cash. Комиссия, списанная с того же счёта, в обороте
        // брокерского отчёта присутствует, и не учесть её значит
        // получить расхождение на ровном месте.
        let account = AccountId::new_random();
        let events = vec![
            event_with(
                account,
                date!(2026 - 03 - 02),
                1,
                EventKind::CashIn {
                    amount: rub(100_000),
                },
                vec![Leg::cash(account, rub(100_000))],
            ),
            event_with(
                account,
                date!(2026 - 03 - 03),
                1,
                EventKind::Fee {
                    amount: rub(-350),
                    origin: FeeOrigin::Brokerage,
                },
                vec![Leg::fee(account, rub(-350))],
            ),
        ];

        let observed = observe(&events, account, march()).unwrap();
        let turnover = observed.turnover(CurrencyCode::Rub).unwrap();
        assert_eq!(turnover.debit, PostedMinor::new(100_000), "приход");
        assert_eq!(turnover.credit, PostedMinor::new(350), "расход модулем");
    }

    #[test]
    fn fees_are_collected_from_trades_too() {
        // Комиссия внутри сделки — та же комиссия. Контрольная секция
        // отчёта суммирует все, и собирать только отдельные события Fee
        // значит недосчитать ровно на комиссиях сделок.
        let account = AccountId::new_random();
        let custody = CustodyId::new_random();
        let instrument = InstrumentId::new_random();
        let quantity = Quantity(Dec::new(Decimal::from(10)));
        let trade = event_with(
            account,
            date!(2026 - 03 - 04),
            1,
            EventKind::Trade {
                side: TradeSide::Buy,
                instrument,
                quantity,
                gross: rub(-50_000),
                fee: Some(rub(-120)),
                accrued_interest: None,
            },
            vec![
                Leg::cash(account, rub(-50_000)),
                Leg::fee(account, rub(-120)),
                Leg::security(account, custody, instrument, quantity),
            ],
        );
        let standalone = event_with(
            account,
            date!(2026 - 03 - 05),
            1,
            EventKind::Fee {
                amount: rub(-80),
                origin: FeeOrigin::Depositary,
            },
            vec![Leg::fee(account, rub(-80))],
        );

        let observed = observe(&[trade, standalone], account, march()).unwrap();
        assert_eq!(
            observed.fees(CurrencyCode::Rub),
            Some(PostedMinor::new(200)),
            "120 внутри сделки плюс 80 отдельным событием, модулем"
        );
    }

    #[test]
    fn an_event_on_the_first_day_belongs_to_the_period_not_before_it() {
        // Граница интервала включается: событие первого марта — это
        // март, а не «до марта». Сдвиг границы на один день перенёс бы
        // операцию в остаток на начало, и обе стороны сверки съехали бы
        // одинаково — то есть ошибка стала бы невидимой.
        let account = AccountId::new_random();
        let events = vec![event_with(
            account,
            date!(2026 - 03 - 01),
            1,
            EventKind::CashIn {
                amount: rub(100_000),
            },
            vec![Leg::cash(account, rub(100_000))],
        )];
        let observed = observe(&events, account, march()).unwrap();
        assert_eq!(
            observed.cash_at(BalancePoint::Opening, CurrencyCode::Rub),
            None,
            "первого марта в остатке на начало марта ещё нет"
        );
        assert_eq!(
            observed.cash_at(BalancePoint::Closing, CurrencyCode::Rub),
            Some(PostedMinor::new(100_000))
        );
        assert_eq!(
            observed.turnover(CurrencyCode::Rub).unwrap().debit,
            PostedMinor::new(100_000),
            "операция первого марта входит в оборот марта"
        );
    }

    #[test]
    fn every_touching_event_is_counted_once() {
        // Счётчик решает, есть ли у счёта история вообще: по нему
        // сверка отличает «не сошлось» от «сверять не с чем».
        let account = AccountId::new_random();
        let events = vec![
            event_with(
                account,
                date!(2026 - 02 - 20),
                1,
                EventKind::CashIn { amount: rub(1) },
                vec![Leg::cash(account, rub(1))],
            ),
            event_with(
                account,
                date!(2026 - 03 - 02),
                1,
                EventKind::CashIn { amount: rub(1) },
                vec![Leg::cash(account, rub(1))],
            ),
            event_with(
                account,
                date!(2026 - 03 - 03),
                1,
                EventKind::CashIn { amount: rub(1) },
                vec![Leg::cash(account, rub(1))],
            ),
        ];
        let observed = observe(&events, account, march()).unwrap();
        assert_eq!(
            observed.events_seen(),
            3,
            "считаются и события до интервала, и события внутри него"
        );
    }

    #[test]
    fn absence_of_movement_is_not_zero() {
        // `None` и `Some(0)` — разные утверждения. Первое означает
        // «данных нет», второе «данные есть, и остаток нулевой».
        // Схлопнуть их значит выдать отсутствие истории за
        // подтверждённый ноль (§4.9, §10.7).
        let account = AccountId::new_random();
        let observed = observe(&[], account, march()).unwrap();
        assert_eq!(
            observed.cash_at(BalancePoint::Closing, CurrencyCode::Rub),
            None
        );
        assert_eq!(observed.turnover(CurrencyCode::Rub), None);
        assert_eq!(observed.events_seen(), 0);
    }

    #[test]
    fn tax_is_not_comparable_until_a_tax_leg_exists() {
        // Ног налога не производит ни один путь записи: налоги — E5.
        // Пока их нет, удержанный налог сравнивать не с чем, и ноль
        // с нашей стороны означает «не считаем», а не «брокер не удержал».
        let account = AccountId::new_random();
        let events = vec![event_with(
            account,
            date!(2026 - 03 - 02),
            1,
            EventKind::CashIn { amount: rub(1) },
            vec![Leg::cash(account, rub(1))],
        )];
        let observed = observe(&events, account, march()).unwrap();
        assert!(!observed.tax_facts_recorded());
        assert_eq!(observed.tax_withheld(CurrencyCode::Rub), None);
    }

    #[test]
    fn a_tax_leg_makes_the_dimension_comparable() {
        // Обратная сторона предыдущего теста: как только налоговый факт
        // появляется, сравнение становится возможным само собой — без
        // правки сверки. Это и есть проверка, что признак считается
        // по журналу, а не зашит константой «в E2 налогов нет».
        let account = AccountId::new_random();
        let events = vec![event_with(
            account,
            date!(2026 - 03 - 07),
            1,
            EventKind::Income {
                instrument: None,
                gross: rub(10_000),
                kind: None,
            },
            vec![
                Leg::cash(account, rub(10_000)),
                Leg::tax(account, rub(-1_300)),
            ],
        )];
        let observed = observe(&events, account, march()).unwrap();
        assert!(observed.tax_facts_recorded());
        assert_eq!(
            observed.tax_withheld(CurrencyCode::Rub),
            Some(PostedMinor::new(1_300)),
            "удержанный налог собирается модулем"
        );
    }

    #[test]
    fn income_is_summed_from_income_events_only() {
        // Приход денег и доход — разные вещи. Пополнение счёта владельцем
        // деньгами является, доходом — нет, и попасть в контрольную сумму
        // купонов и дивидендов не должно.
        let account = AccountId::new_random();
        let events = vec![
            event_with(
                account,
                date!(2026 - 03 - 06),
                1,
                EventKind::CashIn {
                    amount: rub(500_000),
                },
                vec![Leg::cash(account, rub(500_000))],
            ),
            event_with(
                account,
                date!(2026 - 03 - 07),
                1,
                EventKind::Income {
                    instrument: None,
                    gross: rub(4_000),
                    kind: None,
                },
                vec![Leg::cash(account, rub(4_000))],
            ),
        ];
        let observed = observe(&events, account, march()).unwrap();
        assert_eq!(
            observed.income(CurrencyCode::Rub),
            Some(PostedMinor::new(4_000))
        );
    }

    #[test]
    fn another_account_does_not_leak_into_the_totals() {
        // Утверждение делается о счёте. Ноги чужого счёта в обороте —
        // это подтверждение, полученное чужими деньгами.
        let ours = AccountId::new_random();
        let theirs = AccountId::new_random();
        let events = vec![event_with(
            theirs,
            date!(2026 - 03 - 08),
            1,
            EventKind::CashIn { amount: rub(999) },
            vec![Leg::cash(theirs, rub(999))],
        )];
        let observed = observe(&events, ours, march()).unwrap();
        assert_eq!(observed.turnover(CurrencyCode::Rub), None);
        assert_eq!(observed.events_seen(), 0);
    }

    #[test]
    fn an_event_without_a_date_is_a_typed_error() {
        // Событие без даты не попадает ни в один период. Пропустить его
        // молча значит посчитать сверку по неполному срезу и объявить
        // расхождение там, где его нет.
        let account = AccountId::new_random();
        let mut event = event_with(
            account,
            date!(2026 - 03 - 09),
            1,
            EventKind::CashIn { amount: rub(1) },
            vec![Leg::cash(account, rub(1))],
        );
        event.dates = crate::dates::EventDates::empty();
        assert!(matches!(
            observe(&[event], account, march()),
            Err(ObserveError::EventWithoutDate { .. })
        ));
    }
}
