//! Состояние проекции и его отпечаток.
//!
//! Отпечаток нужен не для целостности хранилища, а для того, чтобы
//! `advance` мог отказаться продвигать снимок, который кто-то собрал
//! или изменил мимо ядра (§3.1). Считается по упорядоченным структурам:
//! порядок обхода `BTreeMap` детерминирован, поэтому один и тот же
//! журнал всегда даёт один и тот же отпечаток.

use std::collections::BTreeMap;
use std::collections::BTreeSet;
use std::fmt;

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use time::Date;

use super::balances::Balances;
use super::flows::FlowLog;
use super::income::IncomeLedger;
use super::lots::LotBook;
use crate::event::{Confidence, Event};
use crate::ids::AccountId;
use crate::valuation::PriceBoard;

/// Отпечаток состояния: SHA-256 по упорядоченному обходу.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct StateHash([u8; 32]);

impl StateHash {
    #[must_use]
    pub const fn as_bytes(&self) -> &[u8; 32] {
        &self.0
    }
}

impl fmt::Display for StateHash {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        for byte in &self.0 {
            write!(f, "{byte:02x}")?;
        }
        Ok(())
    }
}

/// Что видел журнал: границы истории и доля непроверенного (§10.7).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Coverage {
    events_applied: u64,
    /// Начало покрытия по каждому счёту.
    ///
    /// Глобальная граница отвечает на вопрос отчёта «с какого дня
    /// вообще есть данные» (§10.7), но для сверки выплат она врёт:
    /// счёт, заведённый восстановленным остатком позже, унаследовал бы
    /// чужое покрытие и получил бы обвинение вместо недоказуемости.
    first_event_by_account: BTreeMap<AccountId, Date>,
    first_event: Option<Date>,
    last_event: Option<Date>,
    /// Счета, история которых начата восстановленным остатком,
    /// а не наблюдаемой операцией.
    restored_accounts: BTreeSet<AccountId>,
    /// События, чьё значение записано как оценка, а не как известный факт
    /// (§4.9). Это **не** уровень сверки: сверка появляется в E2 и живёт
    /// отдельным утверждением о счёте и интервале, а не полем события.
    estimated_events: u64,
}

impl Coverage {
    #[must_use]
    pub const fn events_applied(&self) -> u64 {
        self.events_applied
    }

    /// Дата первого учтённого события. Отчёт обязан её показывать:
    /// «XIRR посчитан с 01.03.2024, ранее данных нет» (§10.7).
    #[must_use]
    pub const fn first_event(&self) -> Option<Date> {
        self.first_event
    }

    /// Начало покрытия конкретного счёта.
    #[must_use]
    pub fn first_event_for(&self, account: AccountId) -> Option<Date> {
        self.first_event_by_account.get(&account).copied()
    }

    #[must_use]
    pub const fn last_event(&self) -> Option<Date> {
        self.last_event
    }

    #[must_use]
    pub fn restored_accounts(&self) -> &BTreeSet<AccountId> {
        &self.restored_accounts
    }

    #[must_use]
    pub const fn estimated_events(&self) -> u64 {
        self.estimated_events
    }

    fn observe(&mut self, event: &Event) {
        self.events_applied += 1;
        if let Some(date) = event.dates.effective_date() {
            self.first_event = Some(match self.first_event {
                Some(existing) => existing.min(date),
                None => date,
            });
            self.first_event_by_account
                .entry(event.account)
                .and_modify(|existing| *existing = (*existing).min(date))
                .or_insert(date);
            self.last_event = Some(match self.last_event {
                Some(existing) => existing.max(date),
                None => date,
            });
        }
        match event.confidence {
            Confidence::Known => {}
            Confidence::Estimated | Confidence::Unknown => self.estimated_events += 1,
        }
        if matches!(
            event.kind,
            crate::event::kind::EventKind::OpeningCash { .. }
                | crate::event::kind::EventKind::OpeningPosition { .. }
        ) {
            self.restored_accounts.insert(event.account);
        }
    }
}

/// Полное состояние проекции.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LedgerState {
    balances: Balances,
    book: LotBook,
    flows: FlowLog,
    income: IncomeLedger,
    prices: PriceBoard,
    coverage: Coverage,
}

impl LedgerState {
    #[must_use]
    pub fn new(book: LotBook) -> Self {
        Self {
            balances: Balances::new(),
            book,
            flows: FlowLog::new(),
            income: IncomeLedger::default(),
            prices: PriceBoard::new(),
            coverage: Coverage::default(),
        }
    }

    #[must_use]
    pub const fn balances(&self) -> &Balances {
        &self.balances
    }

    #[must_use]
    pub const fn book(&self) -> &LotBook {
        &self.book
    }

    #[must_use]
    pub const fn flows(&self) -> &FlowLog {
        &self.flows
    }

    /// Датированные факты дохода: с ними сверяется график выплат.
    #[must_use]
    pub const fn income(&self) -> &IncomeLedger {
        &self.income
    }

    #[must_use]
    pub const fn prices(&self) -> &PriceBoard {
        &self.prices
    }

    #[must_use]
    pub const fn coverage(&self) -> &Coverage {
        &self.coverage
    }

    pub(super) const fn parts_mut(&mut self) -> (&mut Balances, &mut LotBook, &mut FlowLog) {
        (&mut self.balances, &mut self.book, &mut self.flows)
    }

    pub(super) const fn income_mut(&mut self) -> &mut IncomeLedger {
        &mut self.income
    }

    pub(super) const fn prices_mut(&mut self) -> &mut PriceBoard {
        &mut self.prices
    }

    pub(super) fn observe(&mut self, event: &Event) {
        self.coverage.observe(event);
    }

    /// Отпечаток состояния.
    ///
    /// Считается по **канонической сериализации всего состояния**, а не по
    /// перечислению полей вручную. Ручное перечисление проверено ревью
    /// и оказалось неполным: в него не попали реализованный результат,
    /// стоимость приобретений и списаний, версия правила списания
    /// и границы истории. Отпечаток, покрывающий часть состояния, обещает
    /// больше, чем даёт: снимок с изменённым непокрытым полем прошёл бы
    /// проверку. Сериализация покрывает всё, что состояние содержит,
    /// по построению.
    ///
    /// CBOR, а не JSON, по той же причине, что и в хранилище: карты
    /// состояния имеют составные ключи, которые JSON не представляет.
    /// Обход `BTreeMap` детерминирован, `Decimal` сериализуется точно,
    /// двоичной плавающей точки в состоянии нет — поэтому один и тот же
    /// журнал всегда даёт один и тот же отпечаток.
    #[must_use]
    pub fn fingerprint(&self) -> StateHash {
        let mut body = Vec::new();
        // Отказ сериализации здесь невозможен: пишем в вектор в памяти,
        // а состояние состоит из типов, у которых `Serialize` выведен.
        // Тем не менее отпечаток не подменяется заглушкой: одинаковый
        // отпечаток у разных состояний хуже, чем паника.
        ciborium::into_writer(self, &mut body)
            .unwrap_or_else(|error| panic!("состояние не сериализуется: {error}"));
        let mut hasher = Sha256::new();
        hasher.update(b"iaam/ledger-state/v2");
        hasher.update(body);
        StateHash(hasher.finalize().into())
    }
}

/// Отпечаток префикса журнала, свёрнутого в снимок.
///
/// Отвечает на вопрос, на который отпечаток состояния не отвечает:
/// «те ли это события». Событие, добавленное задним числом **до** границы
/// снимка, не меняет ни границу, ни состояние снимка — и без этой
/// проверки просто исчезло бы из расчёта.
///
/// В отпечаток входит каноническое CBOR-тело каждого события, а не только
/// его идентичность. `provenance.raw_hash()` для этой роли не годится:
/// это отпечаток сырого поданного факта, который не меняется, когда
/// приложение выводит производное поле. При этом `raw_hash` обязан
/// оставаться прежним для дедупликации: повтор того же брокерского факта
/// должен остаться дубликатом.
///
/// Отпечаток чувствителен к любому будущему полю [`Event`]. Добавление
/// поля обесценит все снимки и вызовет полный пересчёт. Это осознанная
/// цена: молча посчитать по устаревшему снимку хуже, чем пересчитать.
#[must_use]
pub fn prefix_digest(events: &[&Event]) -> StateHash {
    let mut hasher = Sha256::new();
    hasher.update(b"iaam/journal-prefix/v2");
    hasher.update(
        u64::try_from(events.len())
            .unwrap_or(u64::MAX)
            .to_be_bytes(),
    );
    for event in events {
        // Идентичность подаётся отдельно, хотя тело её и содержит:
        // так покрытие ключевых полей не зависит от того, что однажды
        // сделают с их сериализацией.
        hasher.update(event.id.inner().as_bytes());
        feed_date(&mut hasher, event.order.date());
        hasher.update(event.order.sequence().to_be_bytes());
        let mut body = Vec::new();
        ciborium::into_writer(event, &mut body)
            .expect("событие сериализуемо: обратное — дефект типа, а не данных");
        hasher.update(&body);
    }
    StateHash(hasher.finalize().into())
}

fn feed_date(hasher: &mut Sha256, date: Date) {
    hasher.update(date.year().to_be_bytes());
    hasher.update(date.ordinal().to_be_bytes());
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::event::allocation::{
        AllocationAlgorithmVersion, AllocationEvidence, AllocationInputsHash, BasisAllocation,
    };
    use crate::event::corporate_action::CorporateAction;
    use crate::event::kind::EventKind;
    use crate::event::leg::Leg;
    use crate::event::provenance::{ParserVersion, Provenance, RawHash};
    use crate::event::test_support::event_with;
    use crate::ids::{CustodyId, EventId, InstrumentId, OwnerId, SourceId};
    use crate::money::{CurrencyCode, Money, PerUnitAmount, PostedMinor, Quantity};
    use crate::numeric::decimal::Dec;
    use crate::rules::LotRuleVersion;
    use crate::rules::ReturnedShare;
    use rust_decimal::Decimal;
    use time::OffsetDateTime;
    use time::macros::date;
    use uuid::Uuid;

    fn rub(minor: i64) -> Money {
        Money::new(PostedMinor::new(minor), CurrencyCode::Rub)
    }

    fn cash_in(account: AccountId, day: Date, sequence: u32) -> Event {
        event_with(
            account,
            day,
            sequence,
            EventKind::CashIn {
                amount: rub(10_000),
            },
            vec![Leg::cash(account, rub(10_000))],
        )
    }

    fn known_allocation() -> BasisAllocation {
        BasisAllocation::Known {
            share: ReturnedShare::new(Dec::new(Decimal::new(1, 1)))
                .expect("доля в пределах инварианта"),
            evidence: AllocationEvidence {
                inputs_hash: AllocationInputsHash::new("a".repeat(64)).expect("хеш входов"),
                knowledge_as_of: OffsetDateTime::UNIX_EPOCH,
                algorithm_version: AllocationAlgorithmVersion(1),
            },
        }
    }

    fn amortisation_event(basis_allocation: BasisAllocation) -> Event {
        let account = AccountId(Uuid::from_u128(1));
        let instrument = InstrumentId(Uuid::from_u128(2));
        let custody = CustodyId(Uuid::from_u128(3));
        let mut event = event_with(
            account,
            date!(2026 - 06 - 15),
            5,
            EventKind::CorporateAction {
                action: CorporateAction::PartialRedemption {
                    instrument,
                    custody,
                    quantity: Quantity(Dec::new(Decimal::from(1))),
                    principal_returned_per_unit: PerUnitAmount::new(
                        Dec::new(Decimal::from(100)),
                        CurrencyCode::Rub,
                    ),
                    compensation: rub(10),
                    effective_date: date!(2026 - 06 - 15),
                    record_date: None,
                    grounds: None,
                    basis_allocation,
                },
            },
            vec![Leg::principal(account, instrument, rub(10))],
        );
        event.id = EventId(Uuid::from_u128(4));
        event.owner = OwnerId(Uuid::from_u128(5));
        event.provenance = Provenance::new(
            SourceId(Uuid::from_u128(6)),
            RawHash::parse(&"d".repeat(64)).expect("хеш сырого факта"),
            ParserVersion("test/1".into()),
        );
        event
    }

    #[test]
    fn two_events_differing_only_in_allocation_get_different_digests() {
        let unknown = amortisation_event(BasisAllocation::default());
        let known = amortisation_event(known_allocation());
        assert_ne!(
            prefix_digest(&[&unknown]),
            prefix_digest(&[&known]),
            "отпечаток обязан покрывать содержимое события"
        );
    }

    #[test]
    fn those_same_events_keep_one_raw_hash_so_deduplication_still_works() {
        let unknown = amortisation_event(BasisAllocation::default());
        let known = amortisation_event(known_allocation());
        assert_eq!(
            unknown.provenance.raw_hash(),
            known.provenance.raw_hash(),
            "повтор того же брокерского факта обязан оставаться дубликатом"
        );
    }

    #[test]
    fn a_state_hash_prints_as_lowercase_hex_of_every_byte() {
        // Отпечаток печатается в логах и в ответах API. Пустая строка
        // вместо него неотличима от «отпечатка нет», а обрезанная —
        // от совпадения с другим состоянием.
        let mut bytes = [0_u8; 32];
        bytes[0] = 0x0a;
        bytes[31] = 0xff;
        let printed = StateHash(bytes).to_string();
        assert_eq!(printed.len(), 64);
        assert!(printed.starts_with("0a"), "{printed}");
        assert!(printed.ends_with("ff"), "{printed}");
    }

    #[test]
    fn coverage_counts_events_and_keeps_the_outer_bounds_of_history() {
        // Границы истории — это min и max, а не первое и последнее
        // применённое: события приходят в произвольном порядке.
        let account = AccountId::new_random();
        let mut coverage = Coverage::default();
        assert_eq!(coverage.events_applied(), 0);
        assert_eq!(coverage.first_event(), None);
        assert_eq!(coverage.last_event(), None);

        coverage.observe(&cash_in(account, date!(2025 - 06 - 01), 1));
        coverage.observe(&cash_in(account, date!(2025 - 01 - 15), 2));
        coverage.observe(&cash_in(account, date!(2025 - 12 - 31), 3));

        assert_eq!(coverage.events_applied(), 3);
        assert_eq!(coverage.first_event(), Some(date!(2025 - 01 - 15)));
        assert_eq!(coverage.last_event(), Some(date!(2025 - 12 - 31)));
    }

    #[test]
    fn each_account_carries_its_own_history_horizon() {
        // Глобальный горизонт объявлял бы историю счёта B покрытой
        // с 2020 года только потому, что счёт A существует с 2020-го.
        // Выплаты счёта B за 2021–2025 получали бы обвинение вместо
        // честного «журнал начинается позже графика».
        let a = AccountId::new_random();
        let b = AccountId::new_random();
        let mut coverage = Coverage::default();
        coverage.observe(&cash_in(a, date!(2020 - 01 - 15), 1));
        coverage.observe(&cash_in(b, date!(2026 - 01 - 01), 2));

        assert_eq!(coverage.first_event_for(a), Some(date!(2020 - 01 - 15)));
        assert_eq!(coverage.first_event_for(b), Some(date!(2026 - 01 - 01)));
        // Глобальная граница остаётся: её показывает отчёт о покрытии (§10.7).
        assert_eq!(coverage.first_event(), Some(date!(2020 - 01 - 15)));
    }

    #[test]
    fn coverage_counts_estimated_values_but_not_known_ones() {
        // `Confidence` описывает уверенность в значении (§4.9). Известное
        // значение оценкой не является, и наоборот — иначе доля
        // непроверенного в отчёте перестаёт что-либо означать.
        let account = AccountId::new_random();
        let mut coverage = Coverage::default();
        coverage.observe(&cash_in(account, date!(2025 - 02 - 02), 1));
        assert_eq!(coverage.estimated_events(), 0);

        let mut estimated = cash_in(account, date!(2025 - 02 - 03), 2);
        estimated.confidence = Confidence::Estimated;
        coverage.observe(&estimated);
        assert_eq!(coverage.estimated_events(), 1);

        let mut unknown = cash_in(account, date!(2025 - 02 - 04), 3);
        unknown.confidence = Confidence::Unknown;
        coverage.observe(&unknown);
        assert_eq!(coverage.estimated_events(), 2);
        assert_eq!(coverage.events_applied(), 3);
    }

    #[test]
    fn only_a_restored_opening_marks_the_account_as_restored() {
        // Счёт, история которого начата восстановленным остатком, честно
        // помечается в блоке качества: наблюдения операций до этой даты
        // нет, и доходность за ранний период посчитать нельзя (§10.7).
        let observed = AccountId::new_random();
        let restored = AccountId::new_random();
        let mut coverage = Coverage::default();
        coverage.observe(&cash_in(observed, date!(2025 - 03 - 01), 1));
        assert!(coverage.restored_accounts().is_empty());

        coverage.observe(&event_with(
            restored,
            date!(2025 - 03 - 02),
            2,
            EventKind::OpeningCash {
                amount: rub(50_000),
            },
            vec![Leg::cash(restored, rub(50_000))],
        ));
        assert_eq!(coverage.restored_accounts().len(), 1);
        assert!(coverage.restored_accounts().contains(&restored));
        assert!(!coverage.restored_accounts().contains(&observed));
    }

    #[test]
    fn observing_through_the_state_reaches_its_coverage() {
        // Состояние — единственная дверь к покрытию снаружи: если
        // `observe` перестанет доводить событие до `Coverage`, отчёт
        // о полноте данных станет пустым, а расчёт — нет.
        let account = AccountId::new_random();
        let mut state = LedgerState::new(LotBook::new(LotRuleVersion(1)));
        assert_eq!(state.coverage().events_applied(), 0);
        state.observe(&cash_in(account, date!(2025 - 04 - 04), 1));
        assert_eq!(state.coverage().events_applied(), 1);
        assert_eq!(state.coverage().first_event(), Some(date!(2025 - 04 - 04)));
    }

    #[test]
    fn the_prefix_digest_notices_a_different_date_at_the_same_position() {
        // Дата входит в отпечаток префикса: событие, перенесённое на
        // другой день внутри свёрнутого периода, обязано менять его,
        // иначе `advance` продвинет устаревшее состояние.
        let account = AccountId::new_random();
        let first = cash_in(account, date!(2025 - 05 - 05), 1);
        let mut moved = first.clone();
        moved.order = crate::dates::EffectiveOrder::new(date!(2025 - 05 - 06), 1);

        assert_ne!(prefix_digest(&[&first]), prefix_digest(&[&moved]));
        assert_eq!(prefix_digest(&[&first]), prefix_digest(&[&first]));
        assert_ne!(prefix_digest(&[&first]), prefix_digest(&[]));
    }
}
