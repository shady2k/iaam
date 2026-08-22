//! Envelope события журнала (§4.1).

pub mod kind;
pub mod leg;
pub mod provenance;

use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::dates::{EffectiveOrder, EventDates};
use crate::ids::{AccountId, EventId, OwnerId};
use crate::money::{CurrencyCode, Money, MoneyError};
use kind::{EventKind, TradeSide};
use leg::{Leg, LegKind};
use provenance::Provenance;

/// Уверенность в записанном факте (§4.9).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Confidence {
    /// Факт подтверждён источником.
    Known,
    /// Значение восстановлено или оценено.
    Estimated,
    /// Значение неизвестно и не должно подставляться нулём.
    Unknown,
}

/// Связь с другим событием (§4.8).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Relation {
    /// Самостоятельное событие.
    None,
    /// Сторнирование указанного события.
    Reversal { target: EventId },
    /// Замена указанного события. Всегда идёт после сторнирования.
    Replacement { target: EventId },
}

#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum EventValidationError {
    #[error("для {kind} ожидалось: {expected}; найдено ног: {found}")]
    LegCount {
        kind: &'static str,
        expected: &'static str,
        found: usize,
    },
    #[error("для {kind} знак денежной ноги неверен: {amount} в {currency:?}")]
    WrongSign {
        kind: &'static str,
        amount: i64,
        currency: CurrencyCode,
    },
    #[error("сумма ног ({legs}) не совпадает с суммой события ({declared}) для {kind}")]
    AmountMismatch {
        kind: &'static str,
        legs: i64,
        declared: i64,
    },
    #[error("нога отнесена не к тому счёту: ожидался {expected:?}")]
    WrongAccount { expected: AccountId },
    #[error("две стороны перевода не сходятся: остаток {residual}")]
    TransferResidual { residual: i64 },
    #[error(
        "счёт {account:?} указан и источником, и получателем перевода; \
         перемещение денег внутри одного счёта не меняет ни один остаток \
         и потому не является фактом движения"
    )]
    TransferToSelf { account: AccountId },
    #[error(transparent)]
    Money(#[from] MoneyError),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Sign {
    Positive,
    Negative,
    Any,
}

/// Факт журнала. Неизменяем после записи.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Event {
    pub id: EventId,
    pub schema_version: u32,
    pub owner: OwnerId,
    pub account: AccountId,
    pub kind: EventKind,
    pub dates: EventDates,
    pub order: EffectiveOrder,
    pub legs: Vec<Leg>,
    pub provenance: Provenance,
    pub relation: Relation,
    pub confidence: Confidence,
    /// Ключ идемпотентности от клиента (§10.6).
    pub idempotency_key: Option<String>,
}

/// Текущая версия схемы события.
pub const SCHEMA_VERSION: u32 = 1;

impl Event {
    /// Сумма денежного эффекта всех ног в указанной валюте.
    pub fn cash_effect(&self, currency: CurrencyCode) -> Result<Money, MoneyError> {
        let amounts: Vec<Money> = self
            .legs
            .iter()
            .filter_map(Leg::cash_effect)
            .filter(|m| m.currency() == currency)
            .collect();
        Money::sum(&amounts, currency)
    }

    fn legs_of_kind(&self, kind: LegKind) -> Vec<&Leg> {
        self.legs.iter().filter(|l| l.kind == kind).collect()
    }

    fn cash_legs(&self) -> Vec<&Leg> {
        self.legs_of_kind(LegKind::Cash)
    }

    fn security_legs(&self) -> Vec<&Leg> {
        self.legs_of_kind(LegKind::SecurityQuantity)
    }

    /// Структурная проверка события (§15.2).
    ///
    /// **Не является бухгалтерским балансом.** Ноги события не образуют
    /// двойную запись: контрсчетов капитала, дохода и расхода у них нет.
    /// Поэтому единого правила «сумма ног равна нулю» не существует —
    /// комиссия, записанная одной фактической ногой, никогда не даст ноль,
    /// и это корректно. У каждого типа события своя форма, она и проверяется.
    ///
    /// Тело — только диспетчер по типу события: форма каждого типа проверяется
    /// отдельной функцией, иначе одна ветка молча заимствовала бы условия другой.
    pub fn validate_structure(&self) -> Result<(), EventValidationError> {
        let name = self.kind.discriminant();
        match &self.kind {
            EventKind::CashIn { amount } => self.expect_single_cash(name, *amount, Sign::Positive),
            EventKind::CashOut { amount } => self.expect_single_cash(name, *amount, Sign::Negative),
            EventKind::OpeningCash { amount } => self.expect_single_cash(name, *amount, Sign::Any),
            EventKind::Income { gross, .. } => {
                self.expect_single_cash(name, *gross, Sign::Positive)
            }
            EventKind::Fee { amount, .. } => self.validate_fee(name, *amount),
            EventKind::CashTransfer {
                from, to, amount, ..
            } => self.validate_transfer(name, *from, *to, *amount),
            EventKind::Trade {
                side,
                gross,
                fee,
                accrued_interest,
                ..
            } => self.validate_trade(name, *side, *gross, *fee, *accrued_interest),
            EventKind::OpeningPosition { .. } => self.validate_opening_position(name),
        }
    }

    fn expect_single_cash(
        &self,
        name: &'static str,
        declared: Money,
        sign: Sign,
    ) -> Result<(), EventValidationError> {
        let legs = self.cash_legs();
        let money = single_leg_money(name, &legs, "ровно одна денежная нога")?;
        let raw = money.amount().raw();
        let ok = match sign {
            Sign::Positive => raw > 0,
            Sign::Negative => raw < 0,
            Sign::Any => true,
        };
        if !ok {
            return Err(EventValidationError::WrongSign {
                kind: name,
                amount: raw,
                currency: money.currency(),
            });
        }
        require_equal(name, money, declared)
    }

    /// Комиссия записывается **одной** фактической ногой: контрсчёта расхода
    /// в модели нет, поэтому сумма ног в ноль не сходится, и это корректно.
    fn validate_fee(
        &self,
        name: &'static str,
        declared: Money,
    ) -> Result<(), EventValidationError> {
        let fee_legs = self.legs_of_kind(LegKind::Fee);
        let money = single_leg_money(name, &fee_legs, "ровно одна нога комиссии")?;
        if money.amount().raw() >= 0 {
            return Err(EventValidationError::WrongSign {
                kind: name,
                amount: money.amount().raw(),
                currency: money.currency(),
            });
        }
        require_equal(name, money, declared)
    }

    /// Перевод: две встречные денежные ноги на объявленных счетах.
    fn validate_transfer(
        &self,
        name: &'static str,
        from: AccountId,
        to: AccountId,
        declared: Money,
    ) -> Result<(), EventValidationError> {
        // Проверяется ДО разбора ног. Иначе причина отказа зависела бы от их
        // числа: при двух ногах оба `find` вернули бы одну и ту же, остаток
        // удвоился бы и отказ пришёл бы как `TransferResidual` — по случайной
        // причине; а две нулевые ноги дали бы нулевой остаток и событие
        // прошло бы проверку.
        if from == to {
            return Err(EventValidationError::TransferToSelf { account: from });
        }
        let legs = self.cash_legs();
        if legs.len() != 2 {
            return Err(EventValidationError::LegCount {
                kind: name,
                expected: "ровно две денежные ноги",
                found: legs.len(),
            });
        }
        let out = legs
            .iter()
            .find(|l| l.account == from)
            .ok_or(EventValidationError::WrongAccount { expected: from })?;
        let inn = legs
            .iter()
            .find(|l| l.account == to)
            .ok_or(EventValidationError::WrongAccount { expected: to })?;
        let out_money = leg_money(name, out)?;
        let in_money = leg_money(name, inn)?;
        let residual = out_money.try_add(in_money)?;
        if !residual.is_zero() {
            return Err(EventValidationError::TransferResidual {
                residual: residual.amount().raw(),
            });
        }
        require_equal(name, in_money, declared)
    }

    /// Сделка: ровно одна денежная и ровно одна бумажная нога, денежная
    /// нога равна расчётной сумме со знаком, заданным направлением сделки.
    fn validate_trade(
        &self,
        name: &'static str,
        side: TradeSide,
        gross: Money,
        fee: Option<Money>,
        accrued_interest: Option<Money>,
    ) -> Result<(), EventValidationError> {
        let cash = self.cash_legs();
        let cash_money = single_leg_money(name, &cash, "ровно одна денежная нога")?;
        self.expect_single_security_leg(name)?;
        let expected = trade_settlement(side, gross, fee, accrued_interest)?;
        require_equal(name, cash_money, expected)
    }

    /// Восстановленная позиция описывает только бумагу: денег в этом
    /// событии не двигалось, иначе восстановление остатка выглядело бы
    /// как реальная покупка (§10.7).
    fn validate_opening_position(&self, name: &'static str) -> Result<(), EventValidationError> {
        let cash = self.cash_legs();
        if !cash.is_empty() {
            return Err(EventValidationError::LegCount {
                kind: name,
                expected: "ни одной денежной ноги",
                found: cash.len(),
            });
        }
        self.expect_single_security_leg(name)
    }

    fn expect_single_security_leg(&self, name: &'static str) -> Result<(), EventValidationError> {
        let sec = self.security_legs();
        if sec.len() != 1 {
            return Err(EventValidationError::LegCount {
                kind: name,
                expected: "ровно одна бумажная нога",
                found: sec.len(),
            });
        }
        Ok(())
    }
}

/// Расчётная сумма сделки со знаком денежной ноги (§7.2).
///
/// Тело плюс НКД, затем комиссия: при покупке она увеличивает списание,
/// при продаже уменьшает приход. Знак задаётся направлением сделки —
/// покупка списывает деньги, продажа зачисляет.
fn trade_settlement(
    side: TradeSide,
    gross: Money,
    fee: Option<Money>,
    accrued_interest: Option<Money>,
) -> Result<Money, MoneyError> {
    let mut settlement = gross;
    if let Some(ai) = accrued_interest {
        settlement = settlement.try_add(ai)?;
    }
    match side {
        TradeSide::Buy => {
            let with_fee = match fee {
                Some(f) => settlement.try_add(f)?,
                None => settlement,
            };
            with_fee.checked_negate()
        }
        TradeSide::Sell => match fee {
            Some(f) => settlement.try_sub(f),
            None => Ok(settlement),
        },
    }
}

fn leg_money(name: &'static str, leg: &Leg) -> Result<Money, EventValidationError> {
    leg.money.ok_or(EventValidationError::LegCount {
        kind: name,
        expected: "нога с указанной суммой",
        found: 0,
    })
}

fn single_leg_money(
    name: &'static str,
    legs: &[&Leg],
    expected: &'static str,
) -> Result<Money, EventValidationError> {
    if legs.len() != 1 {
        return Err(EventValidationError::LegCount {
            kind: name,
            expected,
            found: legs.len(),
        });
    }
    leg_money(name, legs[0])
}

fn require_equal(
    name: &'static str,
    leg: Money,
    declared: Money,
) -> Result<(), EventValidationError> {
    if leg.currency() != declared.currency() {
        return Err(EventValidationError::Money(MoneyError::CurrencyMismatch {
            left: leg.currency(),
            right: declared.currency(),
        }));
    }
    if leg.amount().raw() != declared.amount().raw() {
        return Err(EventValidationError::AmountMismatch {
            kind: name,
            legs: leg.amount().raw(),
            declared: declared.amount().raw(),
        });
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::kind::{FeeOrigin, TradeSide};
    use super::provenance::{ParserVersion, RawHash};
    use super::*;
    use crate::dates::CashPostedDate;
    use crate::ids::{CustodyId, InstrumentId, SourceId, TransferId};
    use crate::money::{PostedMinor, Quantity};
    use time::macros::date;

    // Суммы записываются в минимальных единицах одним числом: группировка
    // вида `50_000_00` не компилируется (clippy::inconsistent_digit_grouping
    // входит в `all`, а `all = deny`).
    fn rub(minor: i64) -> Money {
        Money::new(PostedMinor::new(minor), CurrencyCode::Rub)
    }

    fn usd(minor: i64) -> Money {
        Money::new(PostedMinor::new(minor), CurrencyCode::Usd)
    }

    fn event(kind: EventKind, legs: Vec<Leg>, account: AccountId) -> Event {
        Event {
            id: EventId::new_random(),
            schema_version: SCHEMA_VERSION,
            owner: OwnerId::new_random(),
            account,
            kind,
            dates: EventDates::for_cash(CashPostedDate(date!(2026 - 03 - 01))),
            order: EffectiveOrder::new(date!(2026 - 03 - 01), 0),
            legs,
            provenance: Provenance::new(
                SourceId::new_random(),
                RawHash::parse(&"a".repeat(64)).unwrap(),
                ParserVersion("manual/1".into()),
            ),
            relation: Relation::None,
            confidence: Confidence::Known,
            idempotency_key: None,
        }
    }

    fn security_leg(account: AccountId, instrument: InstrumentId) -> Leg {
        Leg::security(
            account,
            CustodyId::new_random(),
            instrument,
            Quantity::zero(),
        )
    }

    // --- Комиссия ---

    #[test]
    fn fee_with_a_single_negative_leg_is_valid() {
        // Комиссия — одна фактическая нога. Сумма ног в ноль не сходится,
        // и это корректно: контрсчёта расхода в модели нет.
        let acc = AccountId::new_random();
        let ev = event(
            EventKind::Fee {
                amount: rub(-3_500),
                origin: FeeOrigin::Brokerage,
            },
            vec![Leg::fee(acc, rub(-3_500))],
            acc,
        );
        assert!(ev.validate_structure().is_ok());
    }

    #[test]
    fn positive_fee_is_rejected() {
        let acc = AccountId::new_random();
        let ev = event(
            EventKind::Fee {
                amount: rub(3_500),
                origin: FeeOrigin::Brokerage,
            },
            vec![Leg::fee(acc, rub(3_500))],
            acc,
        );
        assert!(matches!(
            ev.validate_structure(),
            Err(EventValidationError::WrongSign { .. })
        ));
    }

    #[test]
    fn zero_fee_is_rejected() {
        // Комиссия ноль — не факт о деньгах, а пропущенное поле источника.
        // Граница строгая: `>= 0` отвергает, `> 0` пропустил бы.
        let acc = AccountId::new_random();
        let ev = event(
            EventKind::Fee {
                amount: rub(0),
                origin: FeeOrigin::Depositary,
            },
            vec![Leg::fee(acc, rub(0))],
            acc,
        );
        assert!(matches!(
            ev.validate_structure(),
            Err(EventValidationError::WrongSign { amount: 0, .. })
        ));
    }

    #[test]
    fn fee_leg_must_match_the_declared_amount() {
        let acc = AccountId::new_random();
        let ev = event(
            EventKind::Fee {
                amount: rub(-3_500),
                origin: FeeOrigin::MarginInterest,
            },
            vec![Leg::fee(acc, rub(-3_600))],
            acc,
        );
        assert!(matches!(
            ev.validate_structure(),
            Err(EventValidationError::AmountMismatch {
                legs: -3_600,
                declared: -3_500,
                ..
            })
        ));
    }

    #[test]
    fn fee_needs_exactly_one_fee_leg() {
        let acc = AccountId::new_random();
        let kind = EventKind::Fee {
            amount: rub(-3_500),
            origin: FeeOrigin::Other,
        };
        let none = event(kind.clone(), vec![Leg::cash(acc, rub(-3_500))], acc);
        assert!(matches!(
            none.validate_structure(),
            Err(EventValidationError::LegCount { found: 0, .. })
        ));

        let doubled = event(
            kind,
            vec![Leg::fee(acc, rub(-1_750)), Leg::fee(acc, rub(-1_750))],
            acc,
        );
        assert!(matches!(
            doubled.validate_structure(),
            Err(EventValidationError::LegCount { found: 2, .. })
        ));
    }

    // --- Внешние деньги ---

    #[test]
    fn cash_in_must_be_positive_and_match_the_declared_amount() {
        let acc = AccountId::new_random();
        let ok = event(
            EventKind::CashIn {
                amount: rub(5_000_000),
            },
            vec![Leg::cash(acc, rub(5_000_000))],
            acc,
        );
        assert!(ok.validate_structure().is_ok());

        let negative = event(
            EventKind::CashIn {
                amount: rub(-5_000_000),
            },
            vec![Leg::cash(acc, rub(-5_000_000))],
            acc,
        );
        assert!(matches!(
            negative.validate_structure(),
            Err(EventValidationError::WrongSign { .. })
        ));

        let mismatched = event(
            EventKind::CashIn {
                amount: rub(5_000_000),
            },
            vec![Leg::cash(acc, rub(4_900_000))],
            acc,
        );
        assert!(matches!(
            mismatched.validate_structure(),
            Err(EventValidationError::AmountMismatch { .. })
        ));
    }

    #[test]
    fn zero_cash_in_is_rejected() {
        // Ноль — не приход. Граница строгая: `> 0`, а не `>= 0`.
        let acc = AccountId::new_random();
        let ev = event(
            EventKind::CashIn { amount: rub(0) },
            vec![Leg::cash(acc, rub(0))],
            acc,
        );
        assert!(matches!(
            ev.validate_structure(),
            Err(EventValidationError::WrongSign { amount: 0, .. })
        ));
    }

    #[test]
    fn cash_in_needs_exactly_one_cash_leg() {
        let acc = AccountId::new_random();
        let kind = EventKind::CashIn {
            amount: rub(5_000_000),
        };
        let none = event(kind.clone(), vec![], acc);
        assert!(matches!(
            none.validate_structure(),
            Err(EventValidationError::LegCount { found: 0, .. })
        ));

        let split = event(
            kind,
            vec![
                Leg::cash(acc, rub(5_000_000)),
                Leg::cash(acc, rub(1_000_000)),
            ],
            acc,
        );
        assert!(matches!(
            split.validate_structure(),
            Err(EventValidationError::LegCount { found: 2, .. })
        ));
    }

    #[test]
    fn cash_out_must_be_negative() {
        let acc = AccountId::new_random();
        let ok = event(
            EventKind::CashOut {
                amount: rub(-5_000_000),
            },
            vec![Leg::cash(acc, rub(-5_000_000))],
            acc,
        );
        assert!(ok.validate_structure().is_ok());

        let positive = event(
            EventKind::CashOut {
                amount: rub(5_000_000),
            },
            vec![Leg::cash(acc, rub(5_000_000))],
            acc,
        );
        assert!(matches!(
            positive.validate_structure(),
            Err(EventValidationError::WrongSign { .. })
        ));

        let zero = event(
            EventKind::CashOut { amount: rub(0) },
            vec![Leg::cash(acc, rub(0))],
            acc,
        );
        assert!(matches!(
            zero.validate_structure(),
            Err(EventValidationError::WrongSign { amount: 0, .. })
        ));
    }

    #[test]
    fn opening_cash_accepts_either_sign() {
        // Восстановленный остаток может быть и отрицательным (маржинальный
        // долг), и нулевым: это факт о состоянии, а не о движении.
        let acc = AccountId::new_random();
        for amount in [rub(5_000_000), rub(-5_000_000), rub(0)] {
            let ev = event(
                EventKind::OpeningCash { amount },
                vec![Leg::cash(acc, amount)],
                acc,
            );
            assert!(
                ev.validate_structure().is_ok(),
                "остаток {} должен приниматься",
                amount.amount().raw()
            );
        }
    }

    #[test]
    fn opening_cash_still_must_match_the_declared_amount() {
        let acc = AccountId::new_random();
        let ev = event(
            EventKind::OpeningCash {
                amount: rub(5_000_000),
            },
            vec![Leg::cash(acc, rub(4_900_000))],
            acc,
        );
        assert!(matches!(
            ev.validate_structure(),
            Err(EventValidationError::AmountMismatch { .. })
        ));
    }

    #[test]
    fn income_must_be_a_positive_cash_leg() {
        let acc = AccountId::new_random();
        let ok = event(
            EventKind::Income {
                instrument: Some(InstrumentId::new_random()),
                gross: rub(120_000),
            },
            vec![Leg::cash(acc, rub(120_000))],
            acc,
        );
        assert!(ok.validate_structure().is_ok());

        let negative = event(
            EventKind::Income {
                instrument: None,
                gross: rub(-120_000),
            },
            vec![Leg::cash(acc, rub(-120_000))],
            acc,
        );
        assert!(matches!(
            negative.validate_structure(),
            Err(EventValidationError::WrongSign { .. })
        ));
    }

    // --- Перевод ---

    #[test]
    fn transfer_requires_two_matching_sides() {
        let from = AccountId::new_random();
        let to = AccountId::new_random();
        let kind = EventKind::CashTransfer {
            transfer_id: TransferId::new_random(),
            from,
            to,
            amount: rub(10_000_000),
        };
        let ok = event(
            kind.clone(),
            vec![
                Leg::cash(from, rub(-10_000_000)),
                Leg::cash(to, rub(10_000_000)),
            ],
            from,
        );
        assert!(ok.validate_structure().is_ok());

        // 100 000,00 ушло, 99 000,00 пришло: остаток −1 000,00, то есть
        // −100 000 минимальных единиц.
        let lopsided = event(
            kind,
            vec![
                Leg::cash(from, rub(-10_000_000)),
                Leg::cash(to, rub(9_900_000)),
            ],
            from,
        );
        assert!(matches!(
            lopsided.validate_structure(),
            Err(EventValidationError::TransferResidual { residual: -100_000 })
        ));
    }

    #[test]
    fn transfer_legs_must_sit_on_the_declared_accounts() {
        let from = AccountId::new_random();
        let to = AccountId::new_random();
        let stranger = AccountId::new_random();
        let kind = EventKind::CashTransfer {
            transfer_id: TransferId::new_random(),
            from,
            to,
            amount: rub(10_000_000),
        };
        let wrong_source = event(
            kind.clone(),
            vec![
                Leg::cash(stranger, rub(-10_000_000)),
                Leg::cash(to, rub(10_000_000)),
            ],
            from,
        );
        assert_eq!(
            wrong_source.validate_structure(),
            Err(EventValidationError::WrongAccount { expected: from })
        );

        let wrong_target = event(
            kind,
            vec![
                Leg::cash(from, rub(-10_000_000)),
                Leg::cash(stranger, rub(10_000_000)),
            ],
            from,
        );
        assert_eq!(
            wrong_target.validate_structure(),
            Err(EventValidationError::WrongAccount { expected: to })
        );
    }

    #[test]
    fn transfer_needs_exactly_two_cash_legs() {
        let from = AccountId::new_random();
        let to = AccountId::new_random();
        let kind = EventKind::CashTransfer {
            transfer_id: TransferId::new_random(),
            from,
            to,
            amount: rub(10_000_000),
        };
        let one_sided = event(kind.clone(), vec![Leg::cash(from, rub(-10_000_000))], from);
        assert!(matches!(
            one_sided.validate_structure(),
            Err(EventValidationError::LegCount { found: 1, .. })
        ));

        let three = event(
            kind,
            vec![
                Leg::cash(from, rub(-10_000_000)),
                Leg::cash(to, rub(5_000_000)),
                Leg::cash(to, rub(5_000_000)),
            ],
            from,
        );
        assert!(matches!(
            three.validate_structure(),
            Err(EventValidationError::LegCount { found: 3, .. })
        ));
    }

    #[test]
    fn transfer_to_the_same_account_is_not_a_movement() {
        // Один счёт по обе стороны — не движение денег: ни один остаток
        // не меняется. Отказ приходит по существу, а не потому, что
        // остаток ног случайно не сошёлся.
        let acc = AccountId::new_random();
        let ev = event(
            EventKind::CashTransfer {
                transfer_id: TransferId::new_random(),
                from: acc,
                to: acc,
                amount: rub(10_000_000),
            },
            vec![
                Leg::cash(acc, rub(-10_000_000)),
                Leg::cash(acc, rub(10_000_000)),
            ],
            acc,
        );
        assert_eq!(
            ev.validate_structure(),
            Err(EventValidationError::TransferToSelf { account: acc })
        );
    }

    #[test]
    fn a_transfer_to_self_of_nothing_is_rejected_too() {
        // Вырожденный случай: две нулевые ноги на одном счёте дают нулевой
        // остаток, и проверка сходимости пропустила бы событие. Отказ должен
        // приходить от проверки счетов, а не от арифметики ног.
        let acc = AccountId::new_random();
        let ev = event(
            EventKind::CashTransfer {
                transfer_id: TransferId::new_random(),
                from: acc,
                to: acc,
                amount: rub(0),
            },
            vec![Leg::cash(acc, rub(0)), Leg::cash(acc, rub(0))],
            acc,
        );
        assert_eq!(
            ev.validate_structure(),
            Err(EventValidationError::TransferToSelf { account: acc })
        );
    }

    #[test]
    fn the_self_transfer_check_runs_before_the_legs_are_read() {
        // Ног нет вовсе — отказ всё равно называет настоящую причину,
        // а не «ожидалось ровно две денежные ноги».
        let acc = AccountId::new_random();
        let ev = event(
            EventKind::CashTransfer {
                transfer_id: TransferId::new_random(),
                from: acc,
                to: acc,
                amount: rub(10_000_000),
            },
            vec![],
            acc,
        );
        assert_eq!(
            ev.validate_structure(),
            Err(EventValidationError::TransferToSelf { account: acc })
        );
    }

    #[test]
    fn transfer_amount_must_match_the_incoming_side() {
        let from = AccountId::new_random();
        let to = AccountId::new_random();
        let ev = event(
            EventKind::CashTransfer {
                transfer_id: TransferId::new_random(),
                from,
                to,
                amount: rub(9_000_000),
            },
            vec![
                Leg::cash(from, rub(-10_000_000)),
                Leg::cash(to, rub(10_000_000)),
            ],
            from,
        );
        assert!(matches!(
            ev.validate_structure(),
            Err(EventValidationError::AmountMismatch {
                legs: 10_000_000,
                declared: 9_000_000,
                ..
            })
        ));
    }

    #[test]
    fn a_principal_leg_does_not_disturb_the_transfer_check() {
        // `LegKind::Principal` попадает в `cash_effect`, но проверка перевода
        // смотрит только на ноги вида `Cash`: амортизация номинала не должна
        // выглядеть как третья сторона перевода.
        let from = AccountId::new_random();
        let to = AccountId::new_random();
        let ev = event(
            EventKind::CashTransfer {
                transfer_id: TransferId::new_random(),
                from,
                to,
                amount: rub(10_000_000),
            },
            vec![
                Leg::cash(from, rub(-10_000_000)),
                Leg::cash(to, rub(10_000_000)),
                Leg::principal(from, InstrumentId::new_random(), rub(1)),
            ],
            from,
        );
        assert!(ev.validate_structure().is_ok());
    }

    // --- Сделка ---

    /// Именно этот класс ошибок пропускало прежнее «освобождение
    /// событий с бумажной ногой» от проверки.
    #[test]
    fn buy_with_the_wrong_cash_sign_is_rejected() {
        let acc = AccountId::new_random();
        let instrument = InstrumentId::new_random();
        let kind = EventKind::Trade {
            side: TradeSide::Buy,
            instrument,
            quantity: Quantity::zero(),
            gross: rub(5_000_000),
            fee: Some(rub(3_500)),
            accrued_interest: None,
        };
        // Покупка обязана списывать деньги: −50 035,00.
        let wrong = event(
            kind.clone(),
            vec![
                Leg::cash(acc, rub(5_003_500)),
                security_leg(acc, instrument),
            ],
            acc,
        );
        assert!(matches!(
            wrong.validate_structure(),
            Err(EventValidationError::AmountMismatch { .. })
        ));

        let right = event(
            kind,
            vec![
                Leg::cash(acc, rub(-5_003_500)),
                security_leg(acc, instrument),
            ],
            acc,
        );
        assert!(right.validate_structure().is_ok());
    }

    #[test]
    fn buy_settlement_includes_accrued_interest() {
        // НКД платится продавцу сверх тела: 50 000 + 1 200 + 35 = 51 235.
        let acc = AccountId::new_random();
        let instrument = InstrumentId::new_random();
        let ev = event(
            EventKind::Trade {
                side: TradeSide::Buy,
                instrument,
                quantity: Quantity::zero(),
                gross: rub(5_000_000),
                fee: Some(rub(3_500)),
                accrued_interest: Some(rub(120_000)),
            },
            vec![
                Leg::cash(acc, rub(-5_123_500)),
                security_leg(acc, instrument),
            ],
            acc,
        );
        assert!(ev.validate_structure().is_ok());
    }

    #[test]
    fn sell_settlement_subtracts_the_fee() {
        // Продажа: 50 000 + НКД 1 200 − комиссия 35 = 51 165 приходит.
        let acc = AccountId::new_random();
        let instrument = InstrumentId::new_random();
        let ev = event(
            EventKind::Trade {
                side: TradeSide::Sell,
                instrument,
                quantity: Quantity::zero(),
                gross: rub(5_000_000),
                fee: Some(rub(3_500)),
                accrued_interest: Some(rub(120_000)),
            },
            vec![
                Leg::cash(acc, rub(5_116_500)),
                security_leg(acc, instrument),
            ],
            acc,
        );
        assert!(ev.validate_structure().is_ok());
    }

    #[test]
    fn a_trade_without_a_fee_settles_at_body_plus_accrued_interest() {
        // Комиссии нет — расчётная сумма не должна ни прибавлять,
        // ни вычитать её значение по умолчанию.
        let acc = AccountId::new_random();
        let instrument = InstrumentId::new_random();
        let buy = event(
            EventKind::Trade {
                side: TradeSide::Buy,
                instrument,
                quantity: Quantity::zero(),
                gross: rub(5_000_000),
                fee: None,
                accrued_interest: Some(rub(120_000)),
            },
            vec![
                Leg::cash(acc, rub(-5_120_000)),
                security_leg(acc, instrument),
            ],
            acc,
        );
        assert!(buy.validate_structure().is_ok());

        let sell = event(
            EventKind::Trade {
                side: TradeSide::Sell,
                instrument,
                quantity: Quantity::zero(),
                gross: rub(5_000_000),
                fee: None,
                accrued_interest: None,
            },
            vec![
                Leg::cash(acc, rub(5_000_000)),
                security_leg(acc, instrument),
            ],
            acc,
        );
        assert!(sell.validate_structure().is_ok());
    }

    #[test]
    fn the_fee_moves_the_settlement_in_opposite_directions() {
        // Одна и та же комиссия увеличивает списание при покупке
        // и уменьшает приход при продаже: 50 035 против 49 965.
        let acc = AccountId::new_random();
        let instrument = InstrumentId::new_random();
        let buy_at_sell_amount = event(
            EventKind::Trade {
                side: TradeSide::Buy,
                instrument,
                quantity: Quantity::zero(),
                gross: rub(5_000_000),
                fee: Some(rub(3_500)),
                accrued_interest: None,
            },
            vec![
                Leg::cash(acc, rub(-4_996_500)),
                security_leg(acc, instrument),
            ],
            acc,
        );
        assert!(matches!(
            buy_at_sell_amount.validate_structure(),
            Err(EventValidationError::AmountMismatch {
                legs: -4_996_500,
                declared: -5_003_500,
                ..
            })
        ));

        let sell = event(
            EventKind::Trade {
                side: TradeSide::Sell,
                instrument,
                quantity: Quantity::zero(),
                gross: rub(5_000_000),
                fee: Some(rub(3_500)),
                accrued_interest: None,
            },
            vec![
                Leg::cash(acc, rub(4_996_500)),
                security_leg(acc, instrument),
            ],
            acc,
        );
        assert!(sell.validate_structure().is_ok());
    }

    #[test]
    fn trade_without_a_security_leg_is_rejected() {
        let acc = AccountId::new_random();
        let ev = event(
            EventKind::Trade {
                side: TradeSide::Buy,
                instrument: InstrumentId::new_random(),
                quantity: Quantity::zero(),
                gross: rub(5_000_000),
                fee: None,
                accrued_interest: None,
            },
            vec![Leg::cash(acc, rub(-5_000_000))],
            acc,
        );
        assert!(matches!(
            ev.validate_structure(),
            Err(EventValidationError::LegCount { found: 0, .. })
        ));
    }

    #[test]
    fn trade_with_two_security_legs_is_rejected() {
        let acc = AccountId::new_random();
        let instrument = InstrumentId::new_random();
        let ev = event(
            EventKind::Trade {
                side: TradeSide::Buy,
                instrument,
                quantity: Quantity::zero(),
                gross: rub(5_000_000),
                fee: None,
                accrued_interest: None,
            },
            vec![
                Leg::cash(acc, rub(-5_000_000)),
                security_leg(acc, instrument),
                security_leg(acc, instrument),
            ],
            acc,
        );
        assert!(matches!(
            ev.validate_structure(),
            Err(EventValidationError::LegCount { found: 2, .. })
        ));
    }

    #[test]
    fn trade_without_a_cash_leg_is_rejected() {
        let acc = AccountId::new_random();
        let instrument = InstrumentId::new_random();
        let ev = event(
            EventKind::Trade {
                side: TradeSide::Buy,
                instrument,
                quantity: Quantity::zero(),
                gross: rub(5_000_000),
                fee: None,
                accrued_interest: None,
            },
            vec![security_leg(acc, instrument)],
            acc,
        );
        assert!(matches!(
            ev.validate_structure(),
            Err(EventValidationError::LegCount { found: 0, .. })
        ));
    }

    // --- Восстановленная позиция ---

    #[test]
    fn opening_position_is_a_single_security_leg_without_cash() {
        let acc = AccountId::new_random();
        let instrument = InstrumentId::new_random();
        let ok = event(
            EventKind::OpeningPosition {
                instrument,
                quantity: Quantity::zero(),
                cost_basis: Some(rub(5_000_000)),
            },
            vec![security_leg(acc, instrument)],
            acc,
        );
        assert!(ok.validate_structure().is_ok());
    }

    #[test]
    fn opening_position_with_a_cash_leg_is_rejected() {
        // Восстановление остатка не двигает деньги: иначе оно попало бы
        // в денежный поток как реальная покупка.
        let acc = AccountId::new_random();
        let instrument = InstrumentId::new_random();
        let ev = event(
            EventKind::OpeningPosition {
                instrument,
                quantity: Quantity::zero(),
                cost_basis: None,
            },
            vec![
                Leg::cash(acc, rub(-5_000_000)),
                security_leg(acc, instrument),
            ],
            acc,
        );
        assert!(matches!(
            ev.validate_structure(),
            Err(EventValidationError::LegCount { found: 1, .. })
        ));
    }

    #[test]
    fn opening_position_needs_exactly_one_security_leg() {
        let acc = AccountId::new_random();
        let instrument = InstrumentId::new_random();
        let kind = EventKind::OpeningPosition {
            instrument,
            quantity: Quantity::zero(),
            cost_basis: None,
        };
        let none = event(kind.clone(), vec![], acc);
        assert!(matches!(
            none.validate_structure(),
            Err(EventValidationError::LegCount { found: 0, .. })
        ));

        let two = event(
            kind,
            vec![security_leg(acc, instrument), security_leg(acc, instrument)],
            acc,
        );
        assert!(matches!(
            two.validate_structure(),
            Err(EventValidationError::LegCount { found: 2, .. })
        ));
    }

    // --- Общие правила формы ---

    #[test]
    fn a_leg_of_another_currency_is_not_silently_compared() {
        let acc = AccountId::new_random();
        let ev = event(
            EventKind::CashIn {
                amount: rub(5_000_000),
            },
            vec![Leg::cash(acc, usd(5_000_000))],
            acc,
        );
        assert_eq!(
            ev.validate_structure(),
            Err(EventValidationError::Money(MoneyError::CurrencyMismatch {
                left: CurrencyCode::Usd,
                right: CurrencyCode::Rub,
            }))
        );
    }

    #[test]
    fn a_cash_leg_without_an_amount_is_rejected() {
        // Нога вида `Cash` обязана нести сумму: `None` здесь — не ноль,
        // а отсутствующий факт (§4.9).
        let acc = AccountId::new_random();
        let ev = event(
            EventKind::CashIn {
                amount: rub(5_000_000),
            },
            vec![Leg {
                kind: LegKind::Cash,
                account: acc,
                custody: None,
                instrument: None,
                money: None,
                quantity: None,
            }],
            acc,
        );
        assert!(matches!(
            ev.validate_structure(),
            Err(EventValidationError::LegCount {
                expected: "нога с указанной суммой",
                ..
            })
        ));
    }

    #[test]
    fn a_transfer_leg_without_an_amount_is_rejected() {
        let from = AccountId::new_random();
        let to = AccountId::new_random();
        let ev = event(
            EventKind::CashTransfer {
                transfer_id: TransferId::new_random(),
                from,
                to,
                amount: rub(10_000_000),
            },
            vec![
                Leg {
                    kind: LegKind::Cash,
                    account: from,
                    custody: None,
                    instrument: None,
                    money: None,
                    quantity: None,
                },
                Leg::cash(to, rub(10_000_000)),
            ],
            from,
        );
        assert!(matches!(
            ev.validate_structure(),
            Err(EventValidationError::LegCount {
                expected: "нога с указанной суммой",
                ..
            })
        ));
    }

    // --- Денежный эффект события ---

    #[test]
    fn cash_effect_sums_every_money_bearing_leg() {
        // Покупка на 50 000 с комиссией 35 уменьшает остаток на 50 035:
        // бумажная нога денег не двигает, комиссия — двигает.
        let acc = AccountId::new_random();
        let instrument = InstrumentId::new_random();
        let ev = event(
            EventKind::Trade {
                side: TradeSide::Buy,
                instrument,
                quantity: Quantity::zero(),
                gross: rub(5_000_000),
                fee: Some(rub(3_500)),
                accrued_interest: None,
            },
            vec![
                Leg::cash(acc, rub(-5_000_000)),
                Leg::fee(acc, rub(-3_500)),
                security_leg(acc, instrument),
            ],
            acc,
        );
        assert_eq!(ev.cash_effect(CurrencyCode::Rub), Ok(rub(-5_003_500)));
    }

    #[test]
    fn cash_effect_counts_only_the_requested_currency() {
        // Ноги в разных валютах не складываются и не отбрасывают друг друга:
        // запрошенная валюта выбирается, остальные игнорируются.
        let acc = AccountId::new_random();
        let ev = event(
            EventKind::OpeningCash {
                amount: rub(5_000_000),
            },
            vec![
                Leg::cash(acc, rub(5_000_000)),
                Leg::cash(acc, usd(700_000)),
                Leg::tax(acc, rub(-130_000)),
            ],
            acc,
        );
        assert_eq!(ev.cash_effect(CurrencyCode::Rub), Ok(rub(4_870_000)));
        assert_eq!(ev.cash_effect(CurrencyCode::Usd), Ok(usd(700_000)));
    }

    #[test]
    fn cash_effect_of_an_event_without_money_is_zero_in_that_currency() {
        let acc = AccountId::new_random();
        let instrument = InstrumentId::new_random();
        let ev = event(
            EventKind::OpeningPosition {
                instrument,
                quantity: Quantity::zero(),
                cost_basis: None,
            },
            vec![security_leg(acc, instrument)],
            acc,
        );
        assert_eq!(
            ev.cash_effect(CurrencyCode::Eur),
            Ok(Money::zero(CurrencyCode::Eur))
        );
    }

    // --- Envelope ---

    #[test]
    fn unknown_confidence_is_representable_without_a_placeholder() {
        // Неизвестная уверенность — отдельное значение, а не ноль (§4.9).
        let acc = AccountId::new_random();
        let mut ev = event(
            EventKind::CashIn {
                amount: rub(5_000_000),
            },
            vec![Leg::cash(acc, rub(5_000_000))],
            acc,
        );
        ev.confidence = Confidence::Unknown;
        assert_eq!(ev.confidence, Confidence::Unknown);
        assert_ne!(Confidence::Unknown, Confidence::Estimated);
        assert_ne!(Confidence::Unknown, Confidence::Known);
        // Форма события от уверенности не зависит.
        assert!(ev.validate_structure().is_ok());
    }

    #[test]
    fn an_event_carries_the_current_schema_version() {
        let acc = AccountId::new_random();
        let ev = event(
            EventKind::CashIn {
                amount: rub(5_000_000),
            },
            vec![Leg::cash(acc, rub(5_000_000))],
            acc,
        );
        assert_eq!(ev.schema_version, SCHEMA_VERSION);
        assert_eq!(SCHEMA_VERSION, 1);
    }

    #[test]
    fn a_replacement_points_at_the_event_it_replaces() {
        let target = EventId::new_random();
        assert_ne!(
            Relation::Replacement { target },
            Relation::Reversal { target }
        );
        assert_ne!(Relation::Replacement { target }, Relation::None);
    }
}
