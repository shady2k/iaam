//! Датированные факты дохода (§7.2).
//!
//! Четвёртый независимый читатель журнала. Он намеренно не берёт ничего
//! у лотов: `received_to_date` отвечает на другой вопрос — сколько
//! получено пожизненно, — и делится по лотам пропорционально, а при
//! замещении бумаги переносится на новую. Сверка спрашивает иное:
//! пришла ли конкретная запланированная выплата и когда. Поэтому факт
//! живёт отдельно от агрегатов лота, а гранулярность — ряд на пару
//! (счёт, инструмент).

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use thiserror::Error;
use time::Date;

use crate::event::Event;
use crate::event::kind::{EventKind, IncomeKind};
use crate::ids::EventId;
use crate::money::Money;
use crate::projection::lots::LotKey;
use crate::rules::PostingKind;

/// Факт дохода с датой и видом.
///
/// Хранится `EventId`, а не событие целиком: проекции достаточно ссылки
/// на журнал плюс тех величин, по которым идёт сопоставление. Копия
/// журнала внутри снимка удваивала бы источник истины.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReceivedPosting {
    pub event: EventId,
    pub date: Date,
    pub amount: Money,
    pub kind: PostingKind,
}

/// Почему сверка по паре (счёт, инструмент) недоказуема.
///
/// Это не дефект данных, а честный отказ утверждать: молчаливое
/// «выплата не получена» по неполному входу обвиняет брокера в том,
/// чего журнал не говорит (§4.9).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum IncomeGap {
    /// Есть выплата, вид которой не установлен: на график её не положить.
    IncomeKindUnknown,
    /// Есть выплата без даты зачисления и без даты выплаты.
    PaymentDateUnknown,
}

#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum IncomeError {
    #[error(transparent)]
    Money(#[from] crate::money::MoneyError),
}

/// Датированные факты дохода по парам (счёт, инструмент).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct IncomeLedger {
    entries: BTreeMap<LotKey, Entry>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
struct Entry {
    postings: Vec<ReceivedPosting>,
    gap: Option<IncomeGap>,
}

impl IncomeLedger {
    /// Факты по паре в порядке чтения журнала.
    ///
    /// Пустой срез у пары, которой в карте нет, и у пары без выплат —
    /// одно и то же: «подтверждать нечем». Различает их не срез,
    /// а [`IncomeLedger::gap`].
    #[must_use]
    pub fn postings(&self, key: &LotKey) -> &[ReceivedPosting] {
        self.entries
            .get(key)
            .map_or(&[][..], |entry| entry.postings.as_slice())
    }

    #[must_use]
    pub fn gap(&self, key: &LotKey) -> Option<IncomeGap> {
        self.entries.get(key).and_then(|entry| entry.gap)
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Дата получения денег.
    ///
    /// `cash_posted`, иначе `paid`. Цепочка [`crate::dates::EventDates::effective_date`]
    /// здесь не годится: она начинается с `settled` и падает до `trade`,
    /// а это не даты получения денег — подстановка молча сдвинула бы
    /// факт на другой день (§4.9), и одностороннее окно сопоставления
    /// приняло бы или отвергло его по чужой дате.
    fn payment_date(event: &Event) -> Option<Date> {
        event
            .dates
            .cash_posted
            .map(|posted| posted.0)
            .or_else(|| event.dates.paid.map(|paid| paid.0))
    }

    fn record(&mut self, key: LotKey, posting: ReceivedPosting) {
        self.entries.entry(key).or_default().postings.push(posting);
    }

    /// Пометить пару недоказуемой. Первая причина побеждает: она
    /// возникла раньше в журнале, и перезапись более поздней сделала бы
    /// диагноз функцией порядка чтения, а не содержимого журнала.
    fn mark(&mut self, key: LotKey, gap: IncomeGap) {
        let entry = self.entries.entry(key).or_default();
        if entry.gap.is_none() {
            entry.gap = Some(gap);
        }
    }

    /// Разбор исчерпывающий намеренно: задачи о возврате номинала
    /// и расчёте по оферте добавят свои члены, и компилятор обязан
    /// напомнить о них, а не пропустить событие через `_`.
    pub fn apply(&mut self, event: &Event) -> Result<(), IncomeError> {
        match &event.kind {
            EventKind::Income {
                instrument: Some(instrument),
                gross,
                kind,
            } => {
                let key = LotKey {
                    account: event.account,
                    instrument: *instrument,
                };
                self.apply_income(event, key, *gross, *kind);
                Ok(())
            }
            // Без инструмента сверять не с чем: график выплат
            // принадлежит бумаге.
            EventKind::Income {
                instrument: None, ..
            } => Ok(()),
            // Ни одно из этих событий запланированной выплатой
            // по облигации не является.
            EventKind::Trade { .. }
            | EventKind::CashIn { .. }
            | EventKind::CashOut { .. }
            | EventKind::CashTransfer { .. }
            | EventKind::Fee { .. }
            | EventKind::OpeningPosition { .. }
            | EventKind::OpeningCash { .. }
            | EventKind::Valuation { .. }
            | EventKind::ControlAssertion { .. }
            | EventKind::CorporateAction { .. }
            | EventKind::OfferExercise { .. } => Ok(()),
        }
    }

    fn apply_income(
        &mut self,
        event: &Event,
        key: LotKey,
        amount: Money,
        kind: Option<IncomeKind>,
    ) {
        match kind {
            Some(IncomeKind::Coupon) => {
                let Some(date) = Self::payment_date(event) else {
                    self.mark(key, IncomeGap::PaymentDateUnknown);
                    return;
                };
                self.record(
                    key,
                    ReceivedPosting {
                        event: event.id,
                        date,
                        amount,
                        kind: PostingKind::Coupon,
                    },
                );
            }
            // Дивиденд и процент по вкладу в графике облигации
            // не значатся: подтверждать ими нечего.
            Some(IncomeKind::Dividend | IncomeKind::DepositInterest) => {}
            None => self.mark(key, IncomeGap::IncomeKindUnknown),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dates::{CashPostedDate, EventDates, PaidDate};
    use crate::event::kind::{EventKind, IncomeKind};
    use crate::event::leg::Leg;
    use crate::event::test_support::event_with;
    use crate::ids::{AccountId, InstrumentId};
    use crate::money::{CurrencyCode, Money, PostedMinor};
    use crate::projection::lots::LotKey;
    use crate::rules::PostingKind;
    use time::Date;
    use time::macros::date;

    fn rub(minor: i64) -> Money {
        Money::new(PostedMinor::new(minor), CurrencyCode::Rub)
    }

    /// Событие дохода в конверте `test_support`: он уже проставляет
    /// `cash_posted` тем же днём, что и порядок, — то есть ровно ту дату,
    /// по которой сверка и обязана искать факт.
    fn income(
        account: AccountId,
        instrument: Option<InstrumentId>,
        day: Date,
        kind: Option<IncomeKind>,
        minor: i64,
    ) -> Event {
        let amount = rub(minor);
        event_with(
            account,
            day,
            1,
            EventKind::Income {
                instrument,
                gross: amount,
                kind,
            },
            vec![Leg::cash(account, amount)],
        )
    }

    fn coupon(account: AccountId, instrument: InstrumentId, day: Date, minor: i64) -> Event {
        income(
            account,
            Some(instrument),
            day,
            Some(IncomeKind::Coupon),
            minor,
        )
    }

    /// Купон без единой даты получения денег. `validate_structure` для
    /// `Income` (`event/mod.rs:197`) требует лишь одну положительную
    /// денежную ногу и дат не требует вовсе, поэтому такое событие
    /// приходит из настоящего импорта, а не только из теста.
    fn coupon_without_payment_date(
        account: AccountId,
        instrument: InstrumentId,
        minor: i64,
    ) -> Event {
        let mut event = coupon(account, instrument, date!(2026 - 03 - 18), minor);
        event.dates = EventDates::empty();
        event
    }

    fn income_of_unknown_kind(
        account: AccountId,
        instrument: InstrumentId,
        day: Date,
        minor: i64,
    ) -> Event {
        income(account, Some(instrument), day, None, minor)
    }

    fn dividend(account: AccountId, instrument: InstrumentId, day: Date, minor: i64) -> Event {
        income(
            account,
            Some(instrument),
            day,
            Some(IncomeKind::Dividend),
            minor,
        )
    }

    fn income_without_instrument(account: AccountId, day: Date, minor: i64) -> Event {
        income(account, None, day, Some(IncomeKind::Coupon), minor)
    }

    #[test]
    fn a_coupon_with_a_cash_posted_date_becomes_one_dated_fact() {
        let account = AccountId::new_random();
        let instrument = InstrumentId::new_random();
        let mut ledger = IncomeLedger::default();

        ledger
            .apply(&coupon(account, instrument, date!(2026 - 03 - 18), 500))
            .expect("купон с датой зачисления принимается");

        let key = LotKey {
            account,
            instrument,
        };
        let postings = ledger.postings(&key);
        assert_eq!(postings.len(), 1);
        assert_eq!(postings[0].date, date!(2026 - 03 - 18));
        assert_eq!(postings[0].kind, PostingKind::Coupon);
        assert_eq!(postings[0].amount, rub(500));
        assert_eq!(ledger.gap(&key), None);
    }

    #[test]
    fn a_coupon_falls_back_to_the_paid_date_but_never_to_settled_or_trade() {
        // Цепочка `EventDates::effective_date` начинается с `settled`
        // и падает до `trade` — это не даты получения денег. Взять их
        // значило бы молча сдвинуть факт на другой день (§4.9),
        // а окно сопоставления в правиле — односторонее.
        let account = AccountId::new_random();
        let instrument = InstrumentId::new_random();
        let mut ledger = IncomeLedger::default();

        let mut event = coupon(account, instrument, date!(2026 - 03 - 18), 500);
        event.dates = EventDates {
            settled: Some(crate::dates::SettledDate(date!(2026 - 03 - 10))),
            trade: Some(crate::dates::TradeDate(date!(2026 - 03 - 09))),
            paid: Some(PaidDate(date!(2026 - 03 - 20))),
            ..EventDates::empty()
        };
        ledger.apply(&event).expect("купон принимается");

        let key = LotKey {
            account,
            instrument,
        };
        assert_eq!(ledger.postings(&key)[0].date, date!(2026 - 03 - 20));
    }

    #[test]
    fn a_cash_posted_date_wins_over_the_paid_date() {
        // Деньги на счёте — факт получения; «дата выплаты» эмитентом
        // говорит лишь о том, когда он заплатил.
        let account = AccountId::new_random();
        let instrument = InstrumentId::new_random();
        let mut ledger = IncomeLedger::default();

        let mut event = coupon(account, instrument, date!(2026 - 03 - 18), 500);
        event.dates = EventDates {
            cash_posted: Some(CashPostedDate(date!(2026 - 03 - 18))),
            paid: Some(PaidDate(date!(2026 - 03 - 16))),
            ..EventDates::empty()
        };
        ledger.apply(&event).expect("купон принимается");

        let key = LotKey {
            account,
            instrument,
        };
        assert_eq!(ledger.postings(&key)[0].date, date!(2026 - 03 - 18));
    }

    #[test]
    fn a_payment_without_a_cash_posted_or_paid_date_cannot_be_dated() {
        let account = AccountId::new_random();
        let instrument = InstrumentId::new_random();
        let mut ledger = IncomeLedger::default();

        ledger
            .apply(&coupon_without_payment_date(account, instrument, 500))
            .expect("событие принимается, но датированным фактом не становится");

        let key = LotKey {
            account,
            instrument,
        };
        assert!(ledger.postings(&key).is_empty());
        assert_eq!(ledger.gap(&key), Some(IncomeGap::PaymentDateUnknown));
    }

    #[test]
    fn an_income_of_unknown_kind_blocks_reconciliation_rather_than_being_guessed() {
        let account = AccountId::new_random();
        let instrument = InstrumentId::new_random();
        let mut ledger = IncomeLedger::default();

        ledger
            .apply(&income_of_unknown_kind(
                account,
                instrument,
                date!(2026 - 03 - 18),
                500,
            ))
            .expect("событие принимается");

        let key = LotKey {
            account,
            instrument,
        };
        assert!(ledger.postings(&key).is_empty());
        assert_eq!(ledger.gap(&key), Some(IncomeGap::IncomeKindUnknown));
    }

    #[test]
    fn the_first_reason_a_pair_is_unverifiable_survives_a_later_one() {
        // Диагноз не должен зависеть от того, сколько событий прочитано
        // после первого: перезапись более поздней причиной сделала бы
        // ответ функцией длины журнала, а не его содержимого.
        let account = AccountId::new_random();
        let instrument = InstrumentId::new_random();
        let mut ledger = IncomeLedger::default();

        ledger
            .apply(&coupon_without_payment_date(account, instrument, 500))
            .expect("принимается");
        ledger
            .apply(&income_of_unknown_kind(
                account,
                instrument,
                date!(2026 - 03 - 19),
                700,
            ))
            .expect("принимается");

        let key = LotKey {
            account,
            instrument,
        };
        assert_eq!(ledger.gap(&key), Some(IncomeGap::PaymentDateUnknown));
    }

    #[test]
    fn a_dividend_is_not_a_scheduled_bond_posting() {
        let account = AccountId::new_random();
        let instrument = InstrumentId::new_random();
        let mut ledger = IncomeLedger::default();

        ledger
            .apply(&dividend(account, instrument, date!(2026 - 03 - 18), 500))
            .expect("дивиденд принимается");

        let key = LotKey {
            account,
            instrument,
        };
        assert!(ledger.postings(&key).is_empty());
        assert_eq!(ledger.gap(&key), None);
    }

    #[test]
    fn a_deposit_interest_is_not_a_scheduled_bond_posting() {
        let account = AccountId::new_random();
        let instrument = InstrumentId::new_random();
        let mut ledger = IncomeLedger::default();

        ledger
            .apply(&income(
                account,
                Some(instrument),
                date!(2026 - 03 - 18),
                Some(IncomeKind::DepositInterest),
                500,
            ))
            .expect("процент по вкладу принимается");

        let key = LotKey {
            account,
            instrument,
        };
        assert!(ledger.postings(&key).is_empty());
        assert_eq!(ledger.gap(&key), None);
    }

    #[test]
    fn income_without_an_instrument_has_nothing_to_reconcile_against() {
        let account = AccountId::new_random();
        let mut ledger = IncomeLedger::default();

        ledger
            .apply(&income_without_instrument(
                account,
                date!(2026 - 03 - 18),
                500,
            ))
            .expect("принимается");

        assert!(ledger.is_empty());
    }

    #[test]
    fn two_coupons_on_one_pair_are_two_facts_in_journal_order() {
        // Сверка сопоставляет план с фактами один к одному, поэтому
        // два купона обязаны остаться двумя фактами: их слияние в сумму
        // и есть та потеря, ради которой заведён этот читатель.
        let account = AccountId::new_random();
        let instrument = InstrumentId::new_random();
        let mut ledger = IncomeLedger::default();

        ledger
            .apply(&coupon(account, instrument, date!(2026 - 03 - 18), 500))
            .expect("принимается");
        ledger
            .apply(&coupon(account, instrument, date!(2026 - 09 - 18), 500))
            .expect("принимается");

        let key = LotKey {
            account,
            instrument,
        };
        let postings = ledger.postings(&key);
        assert_eq!(postings.len(), 2);
        assert_eq!(postings[0].date, date!(2026 - 03 - 18));
        assert_eq!(postings[1].date, date!(2026 - 09 - 18));
    }

    #[test]
    fn a_pair_the_journal_never_mentioned_has_neither_facts_nor_a_gap() {
        // «Выплат не было» и «инструмента не видели» — разные ответы;
        // пустой срез без записи в карте отличает их для сверки.
        let ledger = IncomeLedger::default();
        let key = LotKey {
            account: AccountId::new_random(),
            instrument: InstrumentId::new_random(),
        };
        assert!(ledger.postings(&key).is_empty());
        assert_eq!(ledger.gap(&key), None);
        assert!(ledger.is_empty());
    }
}
