//! Денежные потоки границы контура (§4.10, §6.1).
//!
//! Из-за путаницы именно здесь сервисы показывают доходность, в которой
//! собственные пополнения выглядят заработком. Классификацию делает
//! `contour::classify`, этот модуль лишь превращает её в датированный
//! ряд сумм и следит, чтобы знак суммы не противоречил направлению.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use thiserror::Error;
use time::Date;

use crate::contour::{ContourDefinition, ContourId, ContourVersion, FlowClass, classify};
use crate::event::Event;
use crate::ids::EventId;
use crate::money::{CurrencyCode, Money, PostedMinor};

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub enum FlowDirection {
    /// Деньги вошли в контур извне.
    In,
    /// Деньги вышли из контура.
    Out,
}

impl FlowDirection {
    #[must_use]
    pub const fn code(self) -> &'static str {
        match self {
            Self::In => "in",
            Self::Out => "out",
        }
    }
}

/// Поток, пересёкший границу контура.
///
/// Сумма — **проведённая**, в валюте счёта. Перевод в валюту отчёта
/// делается позже и даёт расчётную величину (§3.4).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExternalFlow {
    pub event: EventId,
    pub date: Date,
    pub amount: Money,
    pub direction: FlowDirection,
    pub contour: ContourId,
    pub version: ContourVersion,
}

#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum FlowError {
    #[error("событие {event:?} пересекает границу контура, но не имеет даты")]
    FlowWithoutDate { event: EventId },
    #[error(
        "событие {event:?} классифицировано как {direction:?}, \
         но денежный эффект на счетах контура равен {amount} в {currency:?}"
    )]
    DirectionContradictsAmount {
        event: EventId,
        direction: FlowDirection,
        amount: i64,
        currency: CurrencyCode,
    },
    #[error("переполнение при суммировании ног события {event:?}")]
    Overflow { event: EventId },
}

/// Ряд внешних потоков плюс счётчик внутренних движений.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct FlowLog {
    external: Vec<ExternalFlow>,
    internal: u64,
    irrelevant: u64,
}

impl FlowLog {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    #[must_use]
    pub fn external(&self) -> &[ExternalFlow] {
        &self.external
    }

    /// Число **денежных** движений внутри контура.
    ///
    /// Считаются только события, двигавшие деньги: событие оценки денег
    /// не двигает и движением не является, хотя относится к контуру.
    /// Ноль внешних потоков при ненулевом внутреннем счётчике — законная
    /// ситуация: перевод между своими счетами доходность не меняет (§15.9).
    #[must_use]
    pub const fn internal(&self) -> u64 {
        self.internal
    }

    #[must_use]
    pub const fn irrelevant(&self) -> u64 {
        self.irrelevant
    }

    pub fn apply(&mut self, event: &Event, contour: &ContourDefinition) -> Result<(), FlowError> {
        let (direction, id, version) = match classify(contour, event) {
            FlowClass::ExternalIn { contour, version } => (FlowDirection::In, contour, version),
            FlowClass::ExternalOut { contour, version } => (FlowDirection::Out, contour, version),
            FlowClass::Internal => {
                if moves_money(event) {
                    self.internal += 1;
                }
                return Ok(());
            }
            FlowClass::Irrelevant => {
                if moves_money(event) {
                    self.irrelevant += 1;
                }
                return Ok(());
            }
        };
        let date = event
            .dates
            .effective_date()
            .ok_or(FlowError::FlowWithoutDate { event: event.id })?;
        for (currency, amount) in contour_cash_effect(event, contour)? {
            let money = Money::new(amount, currency);
            require_sign_matches(event.id, direction, money)?;
            self.external.push(ExternalFlow {
                event: event.id,
                date,
                amount: money,
                direction,
                contour: id,
                version,
            });
        }
        Ok(())
    }
}

/// Двигало ли событие деньги хоть где-нибудь.
///
/// Проверяется по ногам, а не по типу события: тип отвечает на вопрос
/// «что произошло», а ноги — «что при этом сдвинулось».
fn moves_money(event: &Event) -> bool {
    event.legs.iter().any(|leg| leg.cash_effect().is_some())
}

/// Денежный эффект события **на счетах контура**, по валютам.
///
/// Для перевода извне внутрь это сумма только входящей ноги: исходящая
/// нога лежит на счёте вне контура и границу не пересекает — она и есть
/// внешний мир.
fn contour_cash_effect(
    event: &Event,
    contour: &ContourDefinition,
) -> Result<BTreeMap<CurrencyCode, PostedMinor>, FlowError> {
    let mut totals: BTreeMap<CurrencyCode, PostedMinor> = BTreeMap::new();
    for leg in &event.legs {
        if !contour.contains(leg.account) {
            continue;
        }
        if let Some(money) = leg.cash_effect() {
            let slot = totals
                .entry(money.currency())
                .or_insert_with(|| PostedMinor::new(0));
            *slot = slot
                .checked_add(money.amount())
                .ok_or(FlowError::Overflow { event: event.id })?;
        }
    }
    totals.retain(|_, amount| amount.raw() != 0);
    Ok(totals)
}

/// Знак суммы обязан соответствовать направлению.
///
/// Расхождение означает, что классификатор и ноги события говорят разное,
/// и молча взять модуль здесь — способ получить доходность, в которой
/// вывод средств выглядит доходом.
fn require_sign_matches(
    event: EventId,
    direction: FlowDirection,
    money: Money,
) -> Result<(), FlowError> {
    let raw = money.amount().raw();
    let ok = match direction {
        FlowDirection::In => raw > 0,
        FlowDirection::Out => raw < 0,
    };
    if ok {
        Ok(())
    } else {
        Err(FlowError::DirectionContradictsAmount {
            event,
            direction,
            amount: raw,
            currency: money.currency(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::contour::ContourVersion;
    use crate::event::kind::EventKind;
    use crate::event::leg::Leg;
    use crate::event::test_support::event_with;
    use crate::ids::{AccountId, EventId, TransferId};
    use crate::money::PostedMinor;
    use time::macros::date;

    fn rub(minor: i64) -> Money {
        Money::new(PostedMinor::new(minor), CurrencyCode::Rub)
    }

    fn contour_of(accounts: [AccountId; 1]) -> ContourDefinition {
        ContourDefinition::new(
            crate::contour::ContourId::new_random(),
            ContourVersion(1),
            accounts,
        )
    }

    fn transfer(from: AccountId, to: AccountId, amount: Money) -> Event {
        event_with(
            from,
            date!(2025 - 05 - 05),
            1,
            EventKind::CashTransfer {
                transfer_id: TransferId::new_random(),
                from,
                to,
                amount,
            },
            vec![
                Leg::cash(from, amount.checked_negate().unwrap()),
                Leg::cash(to, amount),
            ],
        )
    }

    #[test]
    fn an_event_that_moved_no_money_is_not_counted_as_a_movement() {
        // Оценка относится к контуру, но денег не двигает: ног у неё нет.
        // Счётчик внутренних движений — это счётчик движений, а не
        // счётчик событий, иначе «переводов между своими счетами не было»
        // и «была переоценка» становятся неразличимы в блоке качества.
        let account = AccountId::new_random();
        let contour = contour_of([account]);
        let valuation = event_with(
            account,
            date!(2025 - 07 - 07),
            1,
            EventKind::Valuation {
                instrument: crate::ids::InstrumentId::new_random(),
                price: crate::numeric::decimal::Dec::one(),
                currency: CurrencyCode::Rub,
                quality: crate::valuation::PriceQuality::OwnerEstimate,
            },
            vec![],
        );
        let mut log = FlowLog::new();
        log.apply(&valuation, &contour).unwrap();
        assert_eq!(log.internal(), 0);
        assert_eq!(log.external().len(), 0);

        // А перевод между своими счетами — движение, и он считается.
        let other = AccountId::new_random();
        let both = ContourDefinition::new(
            crate::contour::ContourId::new_random(),
            ContourVersion(1),
            [account, other],
        );
        let mut log = FlowLog::new();
        log.apply(&transfer(account, other, rub(10_000)), &both)
            .unwrap();
        assert_eq!(log.internal(), 1);
    }

    #[test]
    fn money_from_outside_is_an_inbound_flow() {
        let account = AccountId::new_random();
        let contour = contour_of([account]);
        let event = event_with(
            account,
            date!(2025 - 01 - 09),
            1,
            EventKind::CashIn {
                amount: rub(50_000),
            },
            vec![Leg::cash(account, rub(50_000))],
        );
        let mut log = FlowLog::new();
        log.apply(&event, &contour).unwrap();
        assert_eq!(log.external().len(), 1);
        assert_eq!(log.external()[0].direction, FlowDirection::In);
        assert_eq!(log.external()[0].amount, rub(50_000));
        assert_eq!(log.external()[0].version, ContourVersion(1));
    }

    #[test]
    fn a_transfer_between_two_accounts_of_the_contour_is_internal() {
        // Именно из-за этой ветки в чужих сервисах перевод со вклада
        // на брокерский счёт выглядит доходом (§4.10).
        let from = AccountId::new_random();
        let to = AccountId::new_random();
        let contour = ContourDefinition::new(
            crate::contour::ContourId::new_random(),
            ContourVersion(1),
            [from, to],
        );
        let mut log = FlowLog::new();
        log.apply(&transfer(from, to, rub(30_000)), &contour)
            .unwrap();
        assert!(log.external().is_empty());
        assert_eq!(log.internal(), 1);
    }

    #[test]
    fn a_transfer_from_outside_carries_only_the_incoming_leg() {
        let outside = AccountId::new_random();
        let inside = AccountId::new_random();
        let contour = contour_of([inside]);
        let mut log = FlowLog::new();
        log.apply(&transfer(outside, inside, rub(30_000)), &contour)
            .unwrap();
        assert_eq!(log.external().len(), 1);
        assert_eq!(log.external()[0].direction, FlowDirection::In);
        assert_eq!(log.external()[0].amount, rub(30_000));
    }

    #[test]
    fn a_purchase_does_not_cross_the_boundary() {
        // Покупка бумаги меняет состав контура, а не его размер.
        let account = AccountId::new_random();
        let contour = contour_of([account]);
        let event = event_with(
            account,
            date!(2025 - 02 - 02),
            1,
            EventKind::Fee {
                amount: rub(-500),
                origin: crate::event::kind::FeeOrigin::Brokerage,
            },
            vec![Leg::fee(account, rub(-500))],
        );
        let mut log = FlowLog::new();
        log.apply(&event, &contour).unwrap();
        assert!(log.external().is_empty());
        assert_eq!(log.internal(), 1);
    }

    #[test]
    fn an_event_outside_the_contour_is_irrelevant_not_external() {
        let inside = AccountId::new_random();
        let outside = AccountId::new_random();
        let contour = contour_of([inside]);
        let event = event_with(
            outside,
            date!(2025 - 03 - 03),
            1,
            EventKind::CashIn { amount: rub(1_000) },
            vec![Leg::cash(outside, rub(1_000))],
        );
        let mut log = FlowLog::new();
        log.apply(&event, &contour).unwrap();
        assert!(log.external().is_empty());
        assert_eq!(log.irrelevant(), 1);
        assert_eq!(log.internal(), 0);
    }

    #[test]
    fn a_direction_that_contradicts_the_sign_is_an_error() {
        // Классификатор сказал «приход», а ноги показывают расход.
        // Взять модуль здесь — способ выдать вывод средств за доход.
        let account = AccountId::new_random();
        let contour = contour_of([account]);
        let mut event = event_with(
            account,
            date!(2025 - 04 - 04),
            1,
            EventKind::CashIn { amount: rub(1_000) },
            vec![Leg::cash(account, rub(1_000))],
        );
        event.legs = vec![Leg::cash(account, rub(-1_000))];
        let mut log = FlowLog::new();
        assert!(matches!(
            log.apply(&event, &contour),
            Err(FlowError::DirectionContradictsAmount { .. })
        ));
    }
    #[test]
    fn directions_have_machine_readable_codes() {
        assert_eq!(FlowDirection::In.code(), "in");
        assert_eq!(FlowDirection::Out.code(), "out");
    }

    #[test]
    fn the_sign_check_is_strict_at_zero() {
        // Нулевая сумма не является ни приходом, ни расходом. Через
        // публичный путь ноль не проходит (нулевые суммы отсеиваются
        // раньше), поэтому граница проверяется прямо на функции —
        // иначе `>` и `>=` здесь неразличимы.
        let event = EventId::new_random();
        assert!(require_sign_matches(event, FlowDirection::In, rub(1)).is_ok());
        assert!(require_sign_matches(event, FlowDirection::In, rub(0)).is_err());
        assert!(require_sign_matches(event, FlowDirection::Out, rub(-1)).is_ok());
        assert!(require_sign_matches(event, FlowDirection::Out, rub(0)).is_err());
    }

    #[test]
    fn irrelevant_events_are_counted_separately_from_internal_ones() {
        let inside = AccountId::new_random();
        let outside = AccountId::new_random();
        let contour = contour_of([inside]);
        let mut log = FlowLog::new();
        for _ in 0..3 {
            let event = event_with(
                outside,
                date!(2025 - 03 - 03),
                1,
                EventKind::CashIn { amount: rub(1_000) },
                vec![Leg::cash(outside, rub(1_000))],
            );
            log.apply(&event, &contour).unwrap();
        }
        assert_eq!(log.irrelevant(), 3);
        assert_eq!(log.internal(), 0);
        assert!(log.external().is_empty());
    }
}
