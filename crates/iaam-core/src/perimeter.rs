//! Периметр: шорты, маржа, РЕПО и ПФИ вне периметра (§11).
//!
//! Граница **возможностная, а не документная**: встретив
//! неподдерживаемую операцию, система не отклоняет отчёт. Наблюдаемый
//! денежный эффект сохраняется всегда; выдумывать экономику
//! неподдерживаемого финансирования система отказывается.
//!
//! Отрицательный денежный остаток поддерживается и в long-only системе:
//! он возникает из-за таймингов расчётов, комиссий и технического
//! овердрафта. В NAV он входит обязательством, а не исчезает.

use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};
use thiserror::Error;
use time::Date;

use crate::event::Event;
use crate::event::kind::{EventKind, FeeOrigin};
use crate::ids::{AccountId, EventId};
use crate::money::{CurrencyCode, PostedMinor};
use crate::reconciliation::Dimension;
use crate::reconciliation::check::ReconciliationException;

/// Политика периметра.
///
/// Окно расчётов задаётся параметром, а не константой: «допустимый
/// срок» (§11) без торгового календаря не вычисляется, а календарь —
/// это E3. Значение по умолчанию покрывает T+2 с выходными.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct PerimeterPolicy {
    pub settlement_window_days: u16,
}

impl Default for PerimeterPolicy {
    fn default() -> Self {
        Self {
            settlement_window_days: 5,
        }
    }
}

/// Классификация отрицательного остатка (§11, таблица).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum NegativeCashClassification {
    /// Закрывается известным расчётом в допустимый срок: расчёты разрешены.
    TemporarySettlementDeficit,
    /// Присутствуют проценты по марже или признак кредита.
    UnsupportedMarginLiability,
    /// Причина неизвестна.
    UnclassifiedNegativeCash,
}

impl NegativeCashClassification {
    #[must_use]
    pub const fn code(self) -> &'static str {
        match self {
            Self::TemporarySettlementDeficit => "temporary_settlement_deficit",
            Self::UnsupportedMarginLiability => "unsupported_margin_liability",
            Self::UnclassifiedNegativeCash => "unclassified_negative_cash",
        }
    }

    /// Блокирует ли классификация налоговые и финансовые отчёты за
    /// период (§11). Временный дефицит расчётов — нет.
    #[must_use]
    pub const fn blocks_reports(self) -> bool {
        match self {
            Self::TemporarySettlementDeficit => false,
            Self::UnsupportedMarginLiability | Self::UnclassifiedNegativeCash => true,
        }
    }
}

/// Промежуток отрицательного остатка.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct NegativeCashSpan {
    pub account: AccountId,
    pub currency: CurrencyCode,
    pub from: Date,
    /// Дата возврата в неотрицательный остаток. `None` — не закрылся.
    pub resolved: Option<Date>,
    pub classification: NegativeCashClassification,
}

#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum PerimeterError {
    #[error("событие {event:?} не имеет даты и не может быть отнесено к промежутку")]
    EventWithoutDate { event: EventId },
    #[error("переполнение остатка счёта {account:?} в {currency:?}")]
    Overflow {
        account: AccountId,
        currency: CurrencyCode,
    },
}

/// Исключения сверки, объяснённые границей периметра.
///
/// Существуют, чтобы владелец не получал задание «починить» то, что
/// система намеренно не поддерживает (§11).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PerimeterExceptions {
    entries: Vec<(AccountId, Dimension, ReconciliationException)>,
}

impl PerimeterExceptions {
    #[must_use]
    pub fn none() -> Self {
        Self::default()
    }

    pub fn add(
        &mut self,
        account: AccountId,
        dimension: Dimension,
        exception: ReconciliationException,
    ) {
        if !self
            .entries
            .iter()
            .any(|(a, d, e)| *a == account && *d == dimension && *e == exception)
        {
            self.entries.push((account, dimension, exception));
        }
    }

    #[must_use]
    pub fn covers(
        &self,
        account: AccountId,
        dimension: Dimension,
    ) -> Option<ReconciliationException> {
        self.entries
            .iter()
            .find(|(a, d, _)| *a == account && *d == dimension)
            .map(|(_, _, exception)| *exception)
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }
}

/// Оценка периметра по журналу.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PerimeterAssessment {
    policy: PerimeterPolicy,
    spans: Vec<NegativeCashSpan>,
    financing: BTreeSet<AccountId>,
}

impl PerimeterAssessment {
    /// Пустая оценка: журнала нет или он не рассматривался.
    #[must_use]
    pub fn empty(policy: PerimeterPolicy) -> Self {
        Self {
            policy,
            spans: Vec::new(),
            financing: BTreeSet::new(),
        }
    }

    #[must_use]
    pub fn spans(&self) -> &[NegativeCashSpan] {
        &self.spans
    }

    #[must_use]
    pub const fn policy(&self) -> PerimeterPolicy {
        self.policy
    }

    /// Есть ли на счёте финансирование вне периметра.
    #[must_use]
    pub fn financing_present(&self, account: AccountId) -> bool {
        self.financing.contains(&account)
    }

    /// Отказываются ли налоговые и финансовые отчёты считаться по этому
    /// счёту.
    ///
    /// Про **другие** счета не говорит ничего: §11 требует, чтобы
    /// остальные продолжали считаться.
    #[must_use]
    pub fn blocks_period_reports(&self, account: AccountId) -> bool {
        self.spans
            .iter()
            .any(|span| span.account == account && span.classification.blocks_reports())
    }

    /// Исключения сверки, следующие из оценки.
    #[must_use]
    pub fn exceptions(&self) -> PerimeterExceptions {
        let mut exceptions = PerimeterExceptions::none();
        for account in &self.financing {
            exceptions.add(
                *account,
                Dimension::Cash,
                ReconciliationException::UnsupportedFinancingPresent,
            );
        }
        exceptions
    }
}

/// Оценка периметра по журналу.
///
/// Логика вынесена из конструктора с именем `new` намеренно (§15.7).
pub fn assess(
    events: &[Event],
    policy: PerimeterPolicy,
) -> Result<PerimeterAssessment, PerimeterError> {
    let mut ordered: Vec<(Date, &Event)> = Vec::with_capacity(events.len());
    for event in events {
        let date = event
            .dates
            .effective_date()
            .ok_or(PerimeterError::EventWithoutDate { event: event.id })?;
        ordered.push((date, event));
    }
    ordered.sort_by_key(|(date, event)| (*date, event.order));

    // Признак кредита собирается по всему журналу заранее: проценты по
    // марже могут быть списаны и после закрытия минуса, но относятся
    // к нему.
    let mut financing: BTreeSet<AccountId> = BTreeSet::new();
    for (_, event) in &ordered {
        if matches!(
            event.kind,
            EventKind::Fee {
                origin: FeeOrigin::MarginInterest,
                ..
            }
        ) {
            financing.insert(event.account);
        }
    }

    let mut balances: BTreeMap<(AccountId, CurrencyCode), PostedMinor> = BTreeMap::new();
    let mut open: BTreeMap<(AccountId, CurrencyCode), Date> = BTreeMap::new();
    let mut spans: Vec<NegativeCashSpan> = Vec::new();

    for (date, event) in &ordered {
        for leg in &event.legs {
            let Some(money) = leg.cash_effect() else {
                continue;
            };
            let key = (leg.account, money.currency());
            let slot = balances.entry(key).or_insert_with(|| PostedMinor::new(0));
            *slot = slot
                .checked_add(money.amount())
                .ok_or(PerimeterError::Overflow {
                    account: leg.account,
                    currency: money.currency(),
                })?;

            let negative = slot.raw() < 0;
            match (negative, open.get(&key).copied()) {
                (true, None) => {
                    open.insert(key, *date);
                }
                (false, Some(start)) => {
                    open.remove(&key);
                    spans.push(classify(key, start, Some(*date), &financing, policy));
                }
                _ => {}
            }
        }
    }
    // Промежутки, не закрывшиеся до конца журнала.
    for (key, start) in open {
        spans.push(classify(key, start, None, &financing, policy));
    }
    spans.sort_by_key(|span| (span.from, span.account, span.currency));
    Ok(PerimeterAssessment {
        policy,
        spans,
        financing,
    })
}

fn classify(
    key: (AccountId, CurrencyCode),
    from: Date,
    resolved: Option<Date>,
    financing: &BTreeSet<AccountId>,
    policy: PerimeterPolicy,
) -> NegativeCashSpan {
    let (account, currency) = key;
    // Порядок ветвей значим: признак кредита сильнее срока. Минус,
    // закрывшийся за день, но сопровождённый процентами по марже,
    // остаётся маржинальным обязательством — экономику финансирования
    // система не достраивает независимо от того, как быстро он закрылся.
    let classification = if financing.contains(&account) {
        NegativeCashClassification::UnsupportedMarginLiability
    } else if resolved
        .is_some_and(|end| (end - from).whole_days() <= i64::from(policy.settlement_window_days))
    {
        NegativeCashClassification::TemporarySettlementDeficit
    } else {
        NegativeCashClassification::UnclassifiedNegativeCash
    };
    NegativeCashSpan {
        account,
        currency,
        from,
        resolved,
        classification,
    }
}
