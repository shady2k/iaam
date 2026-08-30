//! Envelope события журнала (§4.1).

pub mod allocation;
pub mod corporate_action;
pub mod correction;
pub mod kind;
pub mod leg;
pub mod legs;
pub mod offer;
pub mod provenance;

use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::dates::{EffectiveOrder, EventDates};
use crate::ids::{AccountId, CustodyId, EventId, InstrumentId, OwnerId};
use crate::money::{CurrencyCode, Money, MoneyError, Quantity};
use crate::numeric::decimal::Dec;
use corporate_action::{CorporateAction, FractionalTreatment};
use kind::{EventKind, TradeSide};
use leg::{Leg, LegKind};
use legs::LegExpectation;
use offer::OfferExerciseAction;
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
    #[error(
        "для {kind} нога не соответствует событию по полю {field}: \
         событие говорит одно, нога другое"
    )]
    LegDoesNotMatchEvent {
        kind: &'static str,
        field: &'static str,
    },
    #[error("для {kind} величина {field} должна быть положительной, получено {value}")]
    NonPositive {
        kind: &'static str,
        field: &'static str,
        value: String,
    },
    #[error("для {event} лишняя нога: ожидалось ног {expected}, найдено {found}")]
    UnexpectedLeg {
        event: &'static str,
        expected: usize,
        found: usize,
    },
    #[error("для {event} не хватает ноги {kind:?}: ожидалось ног {expected}, найдено {found}")]
    MissingLeg {
        event: &'static str,
        kind: LegKind,
        expected: usize,
        found: usize,
    },
    #[error("для {event} нога {kind:?} не совпала с ожиданием по полю {field}")]
    LegMismatch {
        event: &'static str,
        kind: LegKind,
        field: &'static str,
    },
    #[error(transparent)]
    Numeric(#[from] crate::numeric::NumericError),
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
///
/// Версия 2 отличалась от версии 1 добавленным вариантом
/// [`EventKind::Valuation`]; версия 3 отличается от версии 2
/// добавленным вариантом [`EventKind::ControlAssertion`]; версия 4 —
/// вариантами [`EventKind::CorporateAction`] и
/// [`EventKind::OfferExercise`], а также видом дохода в
/// [`EventKind::Income`] (§4.7). Уже записанные факты прежних версий
/// читаются без изменений — новых вариантов в них просто не
/// встречается, а `Income` без вида читается как «вид не утверждался»,
/// — но программа, знающая только версию 3, не разберёт корпоративное
/// действие и потому не должна притворяться, что разобрала. Оставить
/// прежний номер значило бы, что одна версия обозначает две
/// несовместимые схемы (§4.1).
pub const SCHEMA_VERSION: u32 = 4;

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
                instrument,
                quantity,
                gross,
                fee,
                accrued_interest,
            } => self.validate_trade(
                name,
                *side,
                TradeDeclaration {
                    instrument: *instrument,
                    quantity: *quantity,
                    gross: *gross,
                    fee: *fee,
                    accrued_interest: *accrued_interest,
                },
            ),
            EventKind::OpeningPosition {
                instrument,
                quantity,
                ..
            } => self.validate_opening_position(name, *instrument, *quantity),
            EventKind::Valuation { price, .. } => self.validate_valuation(name, *price),
            EventKind::ControlAssertion { period, claim } => {
                self.validate_control_assertion(name, *period, *claim)
            }
            EventKind::CorporateAction { action } => self.validate_corporate_action(name, action),
            EventKind::OfferExercise { action } => self.validate_offer_exercise(name, action),
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
    /// нога равна расчётной сумме со знаком направления, **а бумажная
    /// нога говорит ровно то же, что тип события**.
    ///
    /// Последнее — не педантизм. Без этой сверки событие «куплено сто
    /// бумаг X», чья нога зачисляет одну бумагу Y на чужой счёт, проходит
    /// проверку и попадает в append-only журнал навсегда. Инвариант
    /// проекции остановит отчёт, но исправить записанный факт можно будет
    /// только сторнированием: входной заслон обязан не пропускать
    /// противоречие, а не сохранять его (§4.3, §4.8).
    fn validate_trade(
        &self,
        name: &'static str,
        side: TradeSide,
        declared: TradeDeclaration,
    ) -> Result<(), EventValidationError> {
        let TradeDeclaration {
            instrument,
            quantity,
            gross,
            fee,
            accrued_interest,
        } = declared;
        require_positive(name, "gross", gross.amount().raw())?;
        require_positive_quantity(name, "quantity", quantity)?;

        let cash = self.cash_legs();
        let cash_money = single_leg_money(name, &cash, "ровно одна денежная нога")?;
        require_own_account(name, cash[0].account, self.account)?;
        let expected = trade_settlement(side, gross, fee, accrued_interest)?;
        require_equal(name, cash_money, expected)?;

        let security = self.security_legs();
        if security.len() != 1 {
            return Err(EventValidationError::LegCount {
                kind: name,
                expected: "ровно одна бумажная нога",
                found: security.len(),
            });
        }
        let leg = security[0];
        require_own_account(name, leg.account, self.account)?;
        require_same_instrument(name, leg.instrument, instrument)?;

        // Покупка увеличивает позицию, продажа уменьшает. Шорты вне
        // периметра (§11), поэтому знак задан направлением однозначно.
        let expected_quantity = match side {
            TradeSide::Buy => quantity,
            TradeSide::Sell => Quantity(quantity.0.checked_neg()?),
        };
        match leg.quantity {
            Some(actual) if actual == expected_quantity => Ok(()),
            _ => Err(EventValidationError::LegDoesNotMatchEvent {
                kind: name,
                field: "quantity",
            }),
        }
    }

    /// Форма корпоративного действия (§4.7).
    ///
    /// Ноги перечисляются **ровно**: посторонняя нога отклоняется так же,
    /// как недостающая, — событие с движением, которого оно не называет,
    /// не является тем событием, которым назвалось.
    fn validate_corporate_action(
        &self,
        name: &'static str,
        action: &CorporateAction,
    ) -> Result<(), EventValidationError> {
        match action {
            // Амортизация выплачивает деньги, но количество бумаг
            // не меняет (§6.5). Отсюда **одна** нога `Principal` и ни
            // одной бумажной: «количество не уменьшается» становится
            // инвариантом формы, а не обещанием.
            //
            // Пары «Cash + Principal» здесь нет намеренно: `Principal`
            // уже входит в `cash_effect()` (`leg.rs`), и пара дала бы
            // двойной денежный эффект.
            CorporateAction::PartialRedemption {
                instrument,
                quantity,
                principal_returned_per_unit,
                compensation,
                ..
            } => {
                require_positive(name, "compensation", compensation.amount().raw())?;
                require_positive_quantity(name, "quantity", *quantity)?;
                // Возврат номинала проверяется здесь, а не в правиле
                // разнесения: правило считает по безразмерной доле и
                // сырое денежное утверждение события больше не видит.
                require_positive_per_unit(
                    name,
                    "principal_returned_per_unit",
                    *principal_returned_per_unit,
                )?;
                self.expect_legs(
                    name,
                    &[principal_leg(self.account, *instrument, *compensation)],
                )
            }
            // Погашение возвращает номинал целиком, и бумага выбывает.
            // Обнулить остаток и оставить количество — позиция
            // из погашенных бумаг, которой не существует.
            CorporateAction::Redemption {
                instrument,
                custody,
                quantity,
                compensation,
                ..
            } => {
                require_positive(name, "compensation", compensation.amount().raw())?;
                require_positive_quantity(name, "quantity", *quantity)?;
                self.expect_legs(
                    name,
                    &[
                        principal_leg(self.account, *instrument, *compensation),
                        security_leg(
                            self.account,
                            *custody,
                            *instrument,
                            Quantity(quantity.0.checked_neg()?),
                        ),
                    ],
                )
            }
            CorporateAction::Conversion {
                predecessor,
                successor,
                custody,
                ratio,
                quantity_in,
                quantity_out,
                fractional,
                compensation,
                ..
            } => {
                require_positive_quantity(name, "quantity_in", *quantity_in)?;
                require_positive_quantity(name, "quantity_out", *quantity_out)?;
                require_positive_quantity(name, "ratio", Quantity(*ratio))?;
                require_conversion_ratio(name, *ratio, *quantity_in, *quantity_out, *fractional)?;
                require_fraction_compensation(name, *fractional, *compensation)?;
                let mut expected = vec![
                    security_leg(
                        self.account,
                        *custody,
                        *predecessor,
                        Quantity(quantity_in.0.checked_neg()?),
                    ),
                    security_leg(self.account, *custody, *successor, *quantity_out),
                ];
                if let Some(compensation) = compensation {
                    expected.push(cash_leg(self.account, *compensation));
                }
                self.expect_legs(name, &expected)
            }
        }
    }

    /// Форма факта оферты (§3.5).
    fn validate_offer_exercise(
        &self,
        name: &'static str,
        action: &OfferExerciseAction,
    ) -> Result<(), EventValidationError> {
        require_positive_quantity(name, "quantity", action.quantity())?;
        match action {
            // Подача и отзыв ног не имеют: ни денег, ни бумаг они
            // не двигают — как контрольное утверждение. Отсутствие ног —
            // тоже форма, и проверяется она наравне с остальными.
            OfferExerciseAction::Submitted { .. } | OfferExerciseAction::Cancelled { .. } => {
                self.expect_legs(name, &[])
            }
            // Выкуп: деньги и отрицательное количество. Ноги `Principal`
            // нет — бумага выбывает, а не возвращает номинал.
            OfferExerciseAction::Settled {
                submission: _,
                instrument,
                custody,
                quantity,
                gross,
                fee,
                accrued_interest,
            } => {
                require_positive(name, "gross", gross.amount().raw())?;
                let settlement =
                    trade_settlement(TradeSide::Sell, *gross, *fee, *accrued_interest)?;
                self.expect_legs(
                    name,
                    &[
                        cash_leg(self.account, settlement),
                        security_leg(
                            self.account,
                            *custody,
                            *instrument,
                            Quantity(quantity.0.checked_neg()?),
                        ),
                    ],
                )
            }
        }
    }

    /// Восстановленная позиция описывает только бумагу: денег в этом
    /// событии не двигалось, иначе восстановление остатка выглядело бы
    /// как реальная покупка (§10.7).
    fn validate_opening_position(
        &self,
        name: &'static str,
        instrument: InstrumentId,
        quantity: Quantity,
    ) -> Result<(), EventValidationError> {
        require_positive_quantity(name, "quantity", quantity)?;
        let cash = self.cash_legs();
        if !cash.is_empty() {
            return Err(EventValidationError::LegCount {
                kind: name,
                expected: "ни одной денежной ноги",
                found: cash.len(),
            });
        }
        let security = self.security_legs();
        if security.len() != 1 {
            return Err(EventValidationError::LegCount {
                kind: name,
                expected: "ровно одна бумажная нога",
                found: security.len(),
            });
        }
        let leg = security[0];
        require_own_account(name, leg.account, self.account)?;
        require_same_instrument(name, leg.instrument, instrument)?;
        match leg.quantity {
            Some(actual) if actual == quantity => Ok(()),
            _ => Err(EventValidationError::LegDoesNotMatchEvent {
                kind: name,
                field: "quantity",
            }),
        }
    }

    /// Оценка не двигает ни денег, ни бумаг: это утверждение о цене.
    /// Нога здесь означала бы, что кто-то записал переоценку как факт
    /// движения, — а нереализованный результат движением не является.
    fn validate_valuation(
        &self,
        name: &'static str,
        price: crate::numeric::decimal::Dec,
    ) -> Result<(), EventValidationError> {
        // Нулевая и отрицательная цена дают отрицательную стоимость
        // позиции и внешне правдоподобную доходность. Бумага может
        // обесцениться до нуля — но это факт делистинга (E3), а не цена.
        if !price.is_positive() {
            return Err(EventValidationError::NonPositive {
                kind: name,
                field: "price",
                value: price.inner().to_string(),
            });
        }
        if self.legs.is_empty() {
            Ok(())
        } else {
            Err(EventValidationError::LegCount {
                kind: name,
                expected: "ни одной ноги",
                found: self.legs.len(),
            })
        }
    }

    /// Контрольное утверждение: ног нет, интервал корректен, величины,
    /// которые обязаны быть модулями, — неотрицательны.
    ///
    /// Отрицательный денежный остаток пропускается намеренно: это
    /// законное состояние (§11). Отрицательное количество бумаг — нет:
    /// шорты вне периметра, и минус здесь означает либо шорт, либо
    /// перепутанный знак при разборе.
    fn validate_control_assertion(
        &self,
        name: &'static str,
        period: crate::reconciliation::claim::AssertionPeriod,
        claim: crate::reconciliation::claim::ControlClaim,
    ) -> Result<(), EventValidationError> {
        use crate::reconciliation::claim::ControlClaim;

        if !period.is_well_formed() {
            return Err(EventValidationError::NonPositive {
                kind: name,
                field: "period",
                value: format!("{} .. {}", period.from, period.to),
            });
        }
        if let ControlClaim::PositionQuantity { quantity, .. } = claim
            && quantity.0.is_negative()
        {
            return Err(EventValidationError::NonPositive {
                kind: name,
                field: "quantity",
                value: quantity.0.inner().to_string(),
            });
        }
        if let Some((field, value)) = claim.non_negative_field()
            && value < 0
        {
            return Err(EventValidationError::NonPositive {
                kind: name,
                field,
                value: value.to_string(),
            });
        }
        if self.legs.is_empty() {
            Ok(())
        } else {
            Err(EventValidationError::LegCount {
                kind: name,
                expected: "ни одной ноги",
                found: self.legs.len(),
            })
        }
    }
}

/// Расчётная сумма сделки со знаком денежной ноги (§7.2).
///
/// Тело плюс НКД, затем комиссия: при покупке она увеличивает списание,
/// при продаже уменьшает приход. Знак задаётся направлением сделки —
/// покупка списывает деньги, продажа зачисляет.
/// Ожидание ноги непогашенного номинала.
fn principal_leg(account: AccountId, instrument: InstrumentId, money: Money) -> LegExpectation {
    LegExpectation {
        kind: LegKind::Principal,
        account,
        instrument: Some(instrument),
        custody: None,
        money: Some(money),
        quantity: None,
    }
}

/// Ожидание бумажной ноги со знаком.
fn security_leg(
    account: AccountId,
    custody: CustodyId,
    instrument: InstrumentId,
    quantity: Quantity,
) -> LegExpectation {
    LegExpectation {
        kind: LegKind::SecurityQuantity,
        account,
        instrument: Some(instrument),
        custody: Some(custody),
        money: None,
        quantity: Some(quantity),
    }
}

/// Ожидание денежной ноги.
fn cash_leg(account: AccountId, money: Money) -> LegExpectation {
    LegExpectation {
        kind: LegKind::Cash,
        account,
        instrument: None,
        custody: None,
        money: Some(money),
        quantity: None,
    }
}

/// Коэффициент замещения сверяется с парой количеств.
///
/// Без сверки коэффициент — необязательная подпись под числами, а E5
/// именно по нему будет переносить налоговую стоимость. Дробная часть
/// учитывается по тому, что с ней сделали: при выкупе или отбрасывании
/// дроби количество преемника округлено вниз, и требовать точного
/// равенства значило бы отвергать корректные замещения.
fn require_conversion_ratio(
    name: &'static str,
    ratio: Dec,
    quantity_in: Quantity,
    quantity_out: Quantity,
    fractional: FractionalTreatment,
) -> Result<(), EventValidationError> {
    let implied = ratio.checked_mul(quantity_in.0)?;
    let expected = match fractional {
        FractionalTreatment::NotApplicable => implied,
        FractionalTreatment::CashCompensated | FractionalTreatment::RoundedDown => {
            Dec::new(implied.inner().floor())
        }
    };
    if quantity_out.0 == expected {
        Ok(())
    } else {
        Err(EventValidationError::LegDoesNotMatchEvent {
            kind: name,
            field: "ratio",
        })
    }
}

/// Компенсация дробей есть тогда и только тогда, когда дробь выкупили.
fn require_fraction_compensation(
    name: &'static str,
    fractional: FractionalTreatment,
    compensation: Option<Money>,
) -> Result<(), EventValidationError> {
    let expected = match fractional {
        FractionalTreatment::CashCompensated => true,
        // Дробь отброшена или её не возникло — платить не за что.
        FractionalTreatment::RoundedDown | FractionalTreatment::NotApplicable => false,
    };
    if compensation.is_some() == expected {
        Ok(())
    } else {
        Err(EventValidationError::LegDoesNotMatchEvent {
            kind: name,
            field: "compensation",
        })
    }
}

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

/// Объявленные условия сделки. Отдельная структура, потому что порог
/// `too-many-arguments-threshold = 6` действует, а подавлять линт нельзя.
#[derive(Debug, Clone, Copy, PartialEq)]
struct TradeDeclaration {
    instrument: InstrumentId,
    quantity: Quantity,
    gross: Money,
    fee: Option<Money>,
    accrued_interest: Option<Money>,
}

fn require_positive(
    name: &'static str,
    field: &'static str,
    value: i64,
) -> Result<(), EventValidationError> {
    if value > 0 {
        Ok(())
    } else {
        Err(EventValidationError::NonPositive {
            kind: name,
            field,
            value: value.to_string(),
        })
    }
}

fn require_positive_per_unit(
    name: &'static str,
    field: &'static str,
    amount: crate::money::PerUnitAmount,
) -> Result<(), EventValidationError> {
    if amount.value().is_positive() {
        Ok(())
    } else {
        Err(EventValidationError::NonPositive {
            kind: name,
            field,
            value: amount.value().inner().to_string(),
        })
    }
}

fn require_positive_quantity(
    name: &'static str,
    field: &'static str,
    quantity: Quantity,
) -> Result<(), EventValidationError> {
    if quantity.0.is_positive() {
        Ok(())
    } else {
        Err(EventValidationError::NonPositive {
            kind: name,
            field,
            value: quantity.0.inner().to_string(),
        })
    }
}

/// Нога обязана лежать на счёте события: иначе одно событие двигало бы
/// бумаги на чужом счёте, а лоты считались бы по своему.
fn require_own_account(
    name: &'static str,
    leg: AccountId,
    event: AccountId,
) -> Result<(), EventValidationError> {
    if leg == event {
        Ok(())
    } else {
        let _ = name;
        Err(EventValidationError::WrongAccount { expected: event })
    }
}

fn require_same_instrument(
    name: &'static str,
    leg: Option<InstrumentId>,
    declared: InstrumentId,
) -> Result<(), EventValidationError> {
    match leg {
        Some(actual) if actual == declared => Ok(()),
        _ => Err(EventValidationError::LegDoesNotMatchEvent {
            kind: name,
            field: "instrument",
        }),
    }
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

/// Конструкторы событий для тестов. Доступны и другим модулям крейты,
/// поэтому вынесены из приватного `mod tests`.
#[cfg(test)]
pub(crate) mod test_support {
    use super::provenance::{ParserVersion, Provenance, RawHash};
    use super::*;
    use crate::dates::CashPostedDate;
    use crate::ids::SourceId;
    use crate::money::PostedMinor;
    use time::macros::date;

    /// Событие произвольного типа для тестов модулей ядра.
    ///
    /// Существует, чтобы тесты проекций не переписывали конверт события
    /// в каждом модуле: переписанный вручную конверт незаметно расходится
    /// с настоящим, и тест начинает проверять фикстуру, а не код.
    pub(crate) fn event_with(
        account: AccountId,
        day: time::Date,
        sequence: u32,
        kind: EventKind,
        legs: Vec<Leg>,
    ) -> Event {
        let dates = EventDates::for_cash(CashPostedDate(day));
        Event {
            id: EventId::new_random(),
            schema_version: SCHEMA_VERSION,
            owner: OwnerId::new_random(),
            account,
            kind,
            dates,
            order: EffectiveOrder::new(day, sequence),
            legs,
            provenance: Provenance::new(
                SourceId::new_random(),
                RawHash::parse(&"d".repeat(64)).unwrap(),
                ParserVersion("test/1".into()),
            ),
            relation: Relation::None,
            confidence: Confidence::Known,
            idempotency_key: None,
        }
    }

    pub(crate) fn sample_event(sequence: u32) -> Event {
        sample_event_with(sequence, Relation::None)
    }

    pub(crate) fn sample_event_with(sequence: u32, relation: Relation) -> Event {
        let account = AccountId::new_random();
        // Сумма записывается одним числом в минимальных единицах:
        // группировка вида `10_000_00` не компилируется
        // (clippy::inconsistent_digit_grouping входит в `all`, а `all = deny`).
        let amount = Money::new(PostedMinor::new(1_000_000), CurrencyCode::Rub);
        Event {
            id: EventId::new_random(),
            schema_version: SCHEMA_VERSION,
            owner: OwnerId::new_random(),
            account,
            kind: EventKind::CashIn { amount },
            dates: EventDates::for_cash(CashPostedDate(date!(2026 - 03 - 01))),
            order: EffectiveOrder::new(date!(2026 - 03 - 01), sequence),
            legs: vec![Leg::cash(account, amount)],
            provenance: Provenance::new(
                SourceId::new_random(),
                RawHash::parse(&"b".repeat(64)).unwrap(),
                ParserVersion("test/1".into()),
            ),
            relation,
            confidence: Confidence::Known,
            idempotency_key: None,
        }
    }
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

    fn qty(units: i64) -> Quantity {
        Quantity(crate::numeric::decimal::Dec::new(
            rust_decimal::Decimal::from(units),
        ))
    }

    // --- форма новых фактов (§4.7, §3.5) ---

    struct Bond {
        account: AccountId,
        instrument: InstrumentId,
        custody: CustodyId,
    }

    impl Bond {
        fn new() -> Self {
            Self {
                account: AccountId::new_random(),
                instrument: InstrumentId::new_random(),
                custody: CustodyId::new_random(),
            }
        }

        fn per_unit(text: &str) -> crate::money::PerUnitAmount {
            crate::money::PerUnitAmount::new(
                crate::numeric::decimal::Dec::new(
                    rust_decimal::Decimal::from_str_exact(text).unwrap(),
                ),
                CurrencyCode::Rub,
            )
        }

        fn amortisation(&self, legs: Vec<Leg>) -> Event {
            self.amortisation_returning("200", legs)
        }

        fn amortisation_returning(&self, returned: &str, legs: Vec<Leg>) -> Event {
            event(
                EventKind::CorporateAction {
                    action: CorporateAction::PartialRedemption {
                        instrument: self.instrument,
                        custody: self.custody,
                        quantity: qty(10),
                        principal_returned_per_unit: Self::per_unit(returned),
                        compensation: rub(100_000),
                        effective_date: date!(2026 - 06 - 15),
                        record_date: None,
                        grounds: None,
                        basis_allocation: crate::event::allocation::BasisAllocation::default(),
                    },
                },
                legs,
                self.account,
            )
        }

        fn redemption(&self, legs: Vec<Leg>) -> Event {
            event(
                EventKind::CorporateAction {
                    action: CorporateAction::Redemption {
                        instrument: self.instrument,
                        custody: self.custody,
                        quantity: qty(10),
                        principal_returned_per_unit: Self::per_unit("800"),
                        compensation: rub(1_000_000),
                        effective_date: date!(2026 - 12 - 15),
                        record_date: None,
                        grounds: None,
                    },
                },
                legs,
                self.account,
            )
        }

        fn offer_settled(&self, legs: Vec<Leg>) -> Event {
            event(
                EventKind::OfferExercise {
                    action: offer::OfferExerciseAction::Settled {
                        submission: offer::OfferSubmissionId::new_random(),
                        instrument: self.instrument,
                        custody: self.custody,
                        quantity: qty(10),
                        gross: rub(1_000_000),
                        fee: None,
                        accrued_interest: None,
                    },
                },
                legs,
                self.account,
            )
        }
    }

    #[test]
    fn amortisation_carries_one_principal_leg_and_nothing_else() {
        let bond = Bond::new();
        assert_eq!(
            bond.amortisation(vec![Leg::principal(
                bond.account,
                bond.instrument,
                rub(100_000)
            )])
            .validate_structure(),
            Ok(())
        );
    }

    #[test]
    fn a_partial_redemption_returning_nothing_is_rejected() {
        let bond = Bond::new();
        let event = bond.amortisation_returning(
            "0",
            vec![Leg::principal(bond.account, bond.instrument, rub(100_000))],
        );
        // Поле названо явно: отказ по compensation или quantity прошёл
        // бы этот тест, ничего не доказав про возврат номинала.
        assert!(matches!(
            event.validate_structure().unwrap_err(),
            EventValidationError::NonPositive { field, .. } if field == "principal_returned_per_unit"
        ));
    }

    #[test]
    fn a_partial_redemption_returning_a_negative_principal_is_rejected() {
        let bond = Bond::new();
        let event = bond.amortisation_returning(
            "-100",
            vec![Leg::principal(bond.account, bond.instrument, rub(100_000))],
        );
        assert!(matches!(
            event.validate_structure().unwrap_err(),
            EventValidationError::NonPositive { field, .. } if field == "principal_returned_per_unit"
        ));
    }

    #[test]
    fn amortisation_with_a_security_quantity_leg_is_rejected() {
        // §6.5: амортизация выплачивает деньги, но количество не меняет.
        let bond = Bond::new();
        assert!(
            bond.amortisation(vec![
                Leg::principal(bond.account, bond.instrument, rub(100_000)),
                Leg::security(bond.account, bond.custody, bond.instrument, qty(-10)),
            ])
            .validate_structure()
            .is_err()
        );
    }

    #[test]
    fn amortisation_with_a_cash_leg_is_rejected() {
        // `Principal` уже денежная нога: пара дала бы двойной эффект.
        let bond = Bond::new();
        assert!(
            bond.amortisation(vec![
                Leg::principal(bond.account, bond.instrument, rub(100_000)),
                Leg::cash(bond.account, rub(100_000)),
            ])
            .validate_structure()
            .is_err()
        );
    }

    #[test]
    fn a_principal_leg_for_another_bond_is_rejected() {
        let bond = Bond::new();
        assert!(
            bond.amortisation(vec![Leg::principal(
                bond.account,
                InstrumentId::new_random(),
                rub(100_000),
            )])
            .validate_structure()
            .is_err()
        );
    }

    #[test]
    fn a_principal_leg_of_another_amount_is_rejected() {
        let bond = Bond::new();
        assert!(
            bond.amortisation(vec![Leg::principal(
                bond.account,
                bond.instrument,
                rub(99_999)
            )])
            .validate_structure()
            .is_err()
        );
    }

    #[test]
    fn final_redemption_carries_the_principal_and_the_leaving_quantity() {
        let bond = Bond::new();
        assert_eq!(
            bond.redemption(vec![
                Leg::principal(bond.account, bond.instrument, rub(1_000_000)),
                Leg::security(bond.account, bond.custody, bond.instrument, qty(-10)),
            ])
            .validate_structure(),
            Ok(())
        );
    }

    #[test]
    fn final_redemption_without_a_security_leg_is_rejected() {
        // Обнулить номинал и оставить количество — позиция
        // из погашенных бумаг, которой не существует.
        let bond = Bond::new();
        assert!(
            bond.redemption(vec![Leg::principal(
                bond.account,
                bond.instrument,
                rub(1_000_000)
            )])
            .validate_structure()
            .is_err()
        );
    }

    #[test]
    fn final_redemption_with_a_positive_security_leg_is_rejected() {
        // Знак — не описка: положительное количество означает приход
        // бумаги, то есть противоположное движение.
        let bond = Bond::new();
        assert!(
            bond.redemption(vec![
                Leg::principal(bond.account, bond.instrument, rub(1_000_000)),
                Leg::security(bond.account, bond.custody, bond.instrument, qty(10)),
            ])
            .validate_structure()
            .is_err()
        );
    }

    /// Стороны замещения. Отдельная структура: порог
    /// `too-many-arguments-threshold = 6` действует и в тестах.
    #[derive(Debug, Clone, Copy)]
    struct Swap {
        account: AccountId,
        predecessor: InstrumentId,
        successor: InstrumentId,
        custody: CustodyId,
    }

    impl Swap {
        fn new() -> Self {
            Self {
                account: AccountId::new_random(),
                predecessor: InstrumentId::new_random(),
                successor: InstrumentId::new_random(),
                custody: CustodyId::new_random(),
            }
        }
    }

    fn conversion(
        swap: Swap,
        ratio: &str,
        quantity_out: i64,
        fractional: corporate_action::FractionalTreatment,
        compensation: Option<Money>,
        legs: Vec<Leg>,
    ) -> Event {
        event(
            EventKind::CorporateAction {
                action: CorporateAction::Conversion {
                    predecessor: swap.predecessor,
                    successor: swap.successor,
                    custody: swap.custody,
                    ratio: crate::numeric::decimal::Dec::new(
                        rust_decimal::Decimal::from_str_exact(ratio).unwrap(),
                    ),
                    quantity_in: qty(10),
                    quantity_out: qty(quantity_out),
                    fractional,
                    compensation,
                    effective_date: date!(2026 - 09 - 01),
                    record_date: None,
                    grounds: None,
                    basis_transfer: corporate_action::BasisTransferRule::CarryOver,
                },
            },
            legs,
            swap.account,
        )
    }

    #[test]
    fn a_conversion_moves_the_quantity_between_two_instruments() {
        let swap = Swap::new();
        assert_eq!(
            conversion(
                swap,
                "1.5",
                15,
                corporate_action::FractionalTreatment::NotApplicable,
                None,
                vec![
                    Leg::security(swap.account, swap.custody, swap.predecessor, qty(-10)),
                    Leg::security(swap.account, swap.custody, swap.successor, qty(15)),
                ],
            )
            .validate_structure(),
            Ok(())
        );
    }

    #[test]
    fn a_conversion_whose_ratio_contradicts_the_quantities_is_rejected() {
        // Коэффициент — не подпись под числами: E5 переносит по нему
        // налоговую стоимость.
        let swap = Swap::new();
        assert_eq!(
            conversion(
                swap,
                "2",
                15,
                corporate_action::FractionalTreatment::NotApplicable,
                None,
                vec![
                    Leg::security(swap.account, swap.custody, swap.predecessor, qty(-10)),
                    Leg::security(swap.account, swap.custody, swap.successor, qty(15)),
                ],
            )
            .validate_structure(),
            Err(EventValidationError::LegDoesNotMatchEvent {
                kind: "corporate_action",
                field: "ratio",
            })
        );
    }

    #[test]
    fn a_rounded_down_conversion_may_end_below_the_exact_ratio() {
        let swap = Swap::new();
        assert_eq!(
            conversion(
                swap,
                "1.55",
                15,
                corporate_action::FractionalTreatment::RoundedDown,
                None,
                vec![
                    Leg::security(swap.account, swap.custody, swap.predecessor, qty(-10)),
                    Leg::security(swap.account, swap.custody, swap.successor, qty(15)),
                ],
            )
            .validate_structure(),
            Ok(())
        );
    }

    #[test]
    fn a_cash_leg_without_a_bought_out_fraction_is_rejected() {
        // Деньги в замещении бывают только компенсацией дроби.
        let swap = Swap::new();
        assert!(
            conversion(
                swap,
                "1.5",
                15,
                corporate_action::FractionalTreatment::NotApplicable,
                None,
                vec![
                    Leg::security(swap.account, swap.custody, swap.predecessor, qty(-10)),
                    Leg::security(swap.account, swap.custody, swap.successor, qty(15)),
                    Leg::cash(swap.account, rub(500)),
                ],
            )
            .validate_structure()
            .is_err()
        );
    }

    #[test]
    fn a_bought_out_fraction_without_compensation_is_rejected() {
        let swap = Swap::new();
        assert_eq!(
            conversion(
                swap,
                "1.55",
                15,
                corporate_action::FractionalTreatment::CashCompensated,
                None,
                vec![
                    Leg::security(swap.account, swap.custody, swap.predecessor, qty(-10)),
                    Leg::security(swap.account, swap.custody, swap.successor, qty(15)),
                ],
            )
            .validate_structure(),
            Err(EventValidationError::LegDoesNotMatchEvent {
                kind: "corporate_action",
                field: "compensation",
            })
        );
    }

    #[test]
    fn a_bought_out_fraction_carries_its_cash_leg() {
        let swap = Swap::new();
        assert_eq!(
            conversion(
                swap,
                "1.55",
                15,
                corporate_action::FractionalTreatment::CashCompensated,
                Some(rub(500)),
                vec![
                    Leg::security(swap.account, swap.custody, swap.predecessor, qty(-10)),
                    Leg::security(swap.account, swap.custody, swap.successor, qty(15)),
                    Leg::cash(swap.account, rub(500)),
                ],
            )
            .validate_structure(),
            Ok(())
        );
    }

    #[test]
    fn a_submitted_offer_moves_nothing() {
        let bond = Bond::new();
        let submitted = event(
            EventKind::OfferExercise {
                action: offer::OfferExerciseAction::Submitted {
                    submission: offer::OfferSubmissionId::new_random(),
                    window: offer::OfferWindowId::new_random(),
                    instrument: bond.instrument,
                    quantity: qty(10),
                },
            },
            Vec::new(),
            bond.account,
        );
        assert_eq!(submitted.validate_structure(), Ok(()));
    }

    #[test]
    fn a_submitted_offer_with_a_leg_is_rejected() {
        let bond = Bond::new();
        let submitted = event(
            EventKind::OfferExercise {
                action: offer::OfferExerciseAction::Submitted {
                    submission: offer::OfferSubmissionId::new_random(),
                    window: offer::OfferWindowId::new_random(),
                    instrument: bond.instrument,
                    quantity: qty(10),
                },
            },
            vec![Leg::cash(bond.account, rub(1))],
            bond.account,
        );
        assert!(submitted.validate_structure().is_err());
    }

    #[test]
    fn a_settled_offer_carries_cash_and_the_leaving_quantity() {
        let bond = Bond::new();
        assert_eq!(
            bond.offer_settled(vec![
                Leg::cash(bond.account, rub(1_000_000)),
                Leg::security(bond.account, bond.custody, bond.instrument, qty(-10)),
            ])
            .validate_structure(),
            Ok(())
        );
    }

    #[test]
    fn a_settled_offer_has_no_principal_leg() {
        // Бумага выбывает, а не возвращает номинал.
        let bond = Bond::new();
        assert!(
            bond.offer_settled(vec![
                Leg::cash(bond.account, rub(1_000_000)),
                Leg::security(bond.account, bond.custody, bond.instrument, qty(-10)),
                Leg::principal(bond.account, bond.instrument, rub(1)),
            ])
            .validate_structure()
            .is_err()
        );
    }

    fn security_leg(account: AccountId, instrument: InstrumentId, quantity: Quantity) -> Leg {
        Leg::security(account, CustodyId::new_random(), instrument, quantity)
    }

    // --- Общие тестовые конструкторы ---

    #[test]
    fn sample_event_passes_structural_validation() {
        // Конструктор из `test_support` используется другими модулями крейты
        // как «обычное событие». Событие, не проходящее структурную проверку,
        // в этой роли негодно: тесты исправлений опирались бы на факт,
        // который журнал не принял бы.
        assert!(test_support::sample_event(0).validate_structure().is_ok());
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
                kind: None,
            },
            vec![Leg::cash(acc, rub(120_000))],
            acc,
        );
        assert!(ok.validate_structure().is_ok());

        let negative = event(
            EventKind::Income {
                instrument: None,
                gross: rub(-120_000),
                kind: None,
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
            quantity: qty(100),
            gross: rub(5_000_000),
            fee: Some(rub(3_500)),
            accrued_interest: None,
        };
        // Покупка обязана списывать деньги: −50 035,00.
        let wrong = event(
            kind.clone(),
            vec![
                Leg::cash(acc, rub(5_003_500)),
                security_leg(acc, instrument, qty(100)),
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
                security_leg(acc, instrument, qty(100)),
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
                quantity: qty(100),
                gross: rub(5_000_000),
                fee: Some(rub(3_500)),
                accrued_interest: Some(rub(120_000)),
            },
            vec![
                Leg::cash(acc, rub(-5_123_500)),
                security_leg(acc, instrument, qty(100)),
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
                quantity: qty(100),
                gross: rub(5_000_000),
                fee: Some(rub(3_500)),
                accrued_interest: Some(rub(120_000)),
            },
            vec![
                Leg::cash(acc, rub(5_116_500)),
                security_leg(acc, instrument, qty(-100)),
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
                quantity: qty(100),
                gross: rub(5_000_000),
                fee: None,
                accrued_interest: Some(rub(120_000)),
            },
            vec![
                Leg::cash(acc, rub(-5_120_000)),
                security_leg(acc, instrument, qty(100)),
            ],
            acc,
        );
        assert!(buy.validate_structure().is_ok());

        let sell = event(
            EventKind::Trade {
                side: TradeSide::Sell,
                instrument,
                quantity: qty(100),
                gross: rub(5_000_000),
                fee: None,
                accrued_interest: None,
            },
            vec![
                Leg::cash(acc, rub(5_000_000)),
                security_leg(acc, instrument, qty(-100)),
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
                quantity: qty(100),
                gross: rub(5_000_000),
                fee: Some(rub(3_500)),
                accrued_interest: None,
            },
            vec![
                Leg::cash(acc, rub(-4_996_500)),
                security_leg(acc, instrument, qty(100)),
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
                quantity: qty(100),
                gross: rub(5_000_000),
                fee: Some(rub(3_500)),
                accrued_interest: None,
            },
            vec![
                Leg::cash(acc, rub(4_996_500)),
                security_leg(acc, instrument, qty(-100)),
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
                quantity: qty(100),
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
                quantity: qty(100),
                gross: rub(5_000_000),
                fee: None,
                accrued_interest: None,
            },
            vec![
                Leg::cash(acc, rub(-5_000_000)),
                security_leg(acc, instrument, qty(100)),
                security_leg(acc, instrument, qty(100)),
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
                quantity: qty(100),
                gross: rub(5_000_000),
                fee: None,
                accrued_interest: None,
            },
            vec![security_leg(acc, instrument, qty(100))],
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
                quantity: qty(100),
                cost_basis: Some(rub(5_000_000)),
                assertions: kind::OpeningAssertions::default(),
            },
            vec![security_leg(acc, instrument, qty(100))],
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
                quantity: qty(100),
                cost_basis: None,
                assertions: kind::OpeningAssertions::default(),
            },
            vec![
                Leg::cash(acc, rub(-5_000_000)),
                security_leg(acc, instrument, qty(100)),
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
            quantity: qty(100),
            cost_basis: None,
            assertions: kind::OpeningAssertions::default(),
        };
        let none = event(kind.clone(), vec![], acc);
        assert!(matches!(
            none.validate_structure(),
            Err(EventValidationError::LegCount { found: 0, .. })
        ));

        let two = event(
            kind,
            vec![
                security_leg(acc, instrument, qty(100)),
                security_leg(acc, instrument, qty(100)),
            ],
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
                quantity: qty(100),
                gross: rub(5_000_000),
                fee: Some(rub(3_500)),
                accrued_interest: None,
            },
            vec![
                Leg::cash(acc, rub(-5_000_000)),
                Leg::fee(acc, rub(-3_500)),
                security_leg(acc, instrument, qty(100)),
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
                quantity: qty(100),
                cost_basis: None,
                assertions: kind::OpeningAssertions::default(),
            },
            vec![security_leg(acc, instrument, qty(100))],
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
        // Литерал закреплён намеренно: подъём версии схемы обязан быть
        // осознанным решением, а не побочным следствием правки. Каждое
        // изменение этой строки требует ответа на вопрос, читаются ли
        // уже записанные факты прежних версий (§4.1).
        //
        // 1 → 2: добавлен `EventKind::Valuation`.
        // 2 → 3: добавлен `EventKind::ControlAssertion` (§10.3).
        // 3 → 4: добавлены `EventKind::CorporateAction` и
        //        `EventKind::OfferExercise`, а `Income` получил вид (§4.7).
        assert_eq!(SCHEMA_VERSION, 4);
    }

    #[test]
    fn a_valuation_with_a_leg_is_rejected() {
        let mut event = test_support::sample_event(1);
        event.kind = EventKind::Valuation {
            instrument: crate::ids::InstrumentId::new_random(),
            price: crate::numeric::decimal::Dec::one(),
            currency: CurrencyCode::Rub,
            quality: crate::valuation::PriceQuality::OwnerEstimate,
        };
        assert!(matches!(
            event.validate_structure(),
            Err(EventValidationError::LegCount { .. })
        ));
        event.legs = vec![];
        assert!(event.validate_structure().is_ok());
    }

    // --- Сверка ноги с событием и знаки величин ---
    //
    // Каждый отказ проверяется отдельно: без этого мутационный заслон
    // показывает, что проверку можно заменить на `Ok(())` и ни один
    // тест не заметит (проверено — так и было).

    fn buy_with(acc: AccountId, instrument: InstrumentId, quantity: Quantity, leg: Leg) -> Event {
        event(
            EventKind::Trade {
                side: TradeSide::Buy,
                instrument,
                quantity,
                gross: rub(5_000_000),
                fee: None,
                accrued_interest: None,
            },
            vec![Leg::cash(acc, rub(-5_000_000)), leg],
            acc,
        )
    }

    #[test]
    fn a_trade_of_zero_quantity_is_rejected() {
        let acc = AccountId::new_random();
        let instrument = InstrumentId::new_random();
        let ev = buy_with(
            acc,
            instrument,
            Quantity::zero(),
            security_leg(acc, instrument, Quantity::zero()),
        );
        assert!(matches!(
            ev.validate_structure(),
            Err(EventValidationError::NonPositive {
                field: "quantity",
                ..
            })
        ));
    }

    #[test]
    fn a_trade_of_negative_quantity_is_rejected() {
        // Отрицательное количество в покупке — это шорт, а шорты вне
        // периметра (§11): их денежный эффект сохраняется отдельным
        // типом события, а не отрицательной сделкой.
        let acc = AccountId::new_random();
        let instrument = InstrumentId::new_random();
        let ev = buy_with(
            acc,
            instrument,
            qty(-10),
            security_leg(acc, instrument, qty(-10)),
        );
        assert!(matches!(
            ev.validate_structure(),
            Err(EventValidationError::NonPositive {
                field: "quantity",
                ..
            })
        ));
    }

    #[test]
    fn a_trade_of_zero_value_is_rejected() {
        let acc = AccountId::new_random();
        let instrument = InstrumentId::new_random();
        let ev = event(
            EventKind::Trade {
                side: TradeSide::Buy,
                instrument,
                quantity: qty(10),
                gross: rub(0),
                fee: None,
                accrued_interest: None,
            },
            vec![
                Leg::cash(acc, rub(0)),
                security_leg(acc, instrument, qty(10)),
            ],
            acc,
        );
        assert!(matches!(
            ev.validate_structure(),
            Err(EventValidationError::NonPositive { field: "gross", .. })
        ));
    }

    #[test]
    fn a_security_leg_of_another_instrument_is_rejected() {
        let acc = AccountId::new_random();
        let instrument = InstrumentId::new_random();
        let other = InstrumentId::new_random();
        let ev = buy_with(acc, instrument, qty(10), security_leg(acc, other, qty(10)));
        assert!(matches!(
            ev.validate_structure(),
            Err(EventValidationError::LegDoesNotMatchEvent {
                field: "instrument",
                ..
            })
        ));
    }

    #[test]
    fn a_security_leg_on_another_account_is_rejected() {
        let acc = AccountId::new_random();
        let stranger = AccountId::new_random();
        let instrument = InstrumentId::new_random();
        let ev = buy_with(
            acc,
            instrument,
            qty(10),
            security_leg(stranger, instrument, qty(10)),
        );
        assert!(matches!(
            ev.validate_structure(),
            Err(EventValidationError::WrongAccount { .. })
        ));
    }

    #[test]
    fn a_cash_leg_on_another_account_is_rejected() {
        let acc = AccountId::new_random();
        let stranger = AccountId::new_random();
        let instrument = InstrumentId::new_random();
        let ev = event(
            EventKind::Trade {
                side: TradeSide::Buy,
                instrument,
                quantity: qty(10),
                gross: rub(5_000_000),
                fee: None,
                accrued_interest: None,
            },
            vec![
                Leg::cash(stranger, rub(-5_000_000)),
                security_leg(acc, instrument, qty(10)),
            ],
            acc,
        );
        assert!(matches!(
            ev.validate_structure(),
            Err(EventValidationError::WrongAccount { .. })
        ));
    }

    #[test]
    fn a_leg_quantity_differing_from_the_event_is_rejected() {
        let acc = AccountId::new_random();
        let instrument = InstrumentId::new_random();
        let ev = buy_with(
            acc,
            instrument,
            qty(10),
            security_leg(acc, instrument, qty(9)),
        );
        assert!(matches!(
            ev.validate_structure(),
            Err(EventValidationError::LegDoesNotMatchEvent {
                field: "quantity",
                ..
            })
        ));
    }

    #[test]
    fn a_purchase_whose_leg_reduces_the_position_is_rejected() {
        // Знак задан направлением сделки: покупка увеличивает позицию.
        let acc = AccountId::new_random();
        let instrument = InstrumentId::new_random();
        let ev = buy_with(
            acc,
            instrument,
            qty(10),
            security_leg(acc, instrument, qty(-10)),
        );
        assert!(matches!(
            ev.validate_structure(),
            Err(EventValidationError::LegDoesNotMatchEvent {
                field: "quantity",
                ..
            })
        ));
    }

    #[test]
    fn a_sale_whose_leg_increases_the_position_is_rejected() {
        let acc = AccountId::new_random();
        let instrument = InstrumentId::new_random();
        let ev = event(
            EventKind::Trade {
                side: TradeSide::Sell,
                instrument,
                quantity: qty(10),
                gross: rub(5_000_000),
                fee: None,
                accrued_interest: None,
            },
            vec![
                Leg::cash(acc, rub(5_000_000)),
                security_leg(acc, instrument, qty(10)),
            ],
            acc,
        );
        assert!(matches!(
            ev.validate_structure(),
            Err(EventValidationError::LegDoesNotMatchEvent {
                field: "quantity",
                ..
            })
        ));
    }

    #[test]
    fn an_opening_position_disagreeing_with_its_leg_is_rejected() {
        let acc = AccountId::new_random();
        let instrument = InstrumentId::new_random();
        let other = InstrumentId::new_random();

        let wrong_quantity = event(
            EventKind::OpeningPosition {
                instrument,
                quantity: qty(10),
                cost_basis: None,
                assertions: kind::OpeningAssertions::default(),
            },
            vec![security_leg(acc, instrument, qty(11))],
            acc,
        );
        assert!(matches!(
            wrong_quantity.validate_structure(),
            Err(EventValidationError::LegDoesNotMatchEvent {
                field: "quantity",
                ..
            })
        ));

        let wrong_instrument = event(
            EventKind::OpeningPosition {
                instrument,
                quantity: qty(10),
                cost_basis: None,
                assertions: kind::OpeningAssertions::default(),
            },
            vec![security_leg(acc, other, qty(10))],
            acc,
        );
        assert!(matches!(
            wrong_instrument.validate_structure(),
            Err(EventValidationError::LegDoesNotMatchEvent {
                field: "instrument",
                ..
            })
        ));

        let zero = event(
            EventKind::OpeningPosition {
                instrument,
                quantity: Quantity::zero(),
                cost_basis: None,
                assertions: kind::OpeningAssertions::default(),
            },
            vec![security_leg(acc, instrument, Quantity::zero())],
            acc,
        );
        assert!(matches!(
            zero.validate_structure(),
            Err(EventValidationError::NonPositive {
                field: "quantity",
                ..
            })
        ));
    }

    #[test]
    fn a_valuation_at_zero_or_below_is_rejected() {
        // Нулевая цена даёт нулевую стоимость позиции и правдоподобную
        // доходность. Обесценившаяся бумага — это факт делистинга (E3),
        // а не цена.
        let acc = AccountId::new_random();
        for price in [
            crate::numeric::decimal::Dec::zero(),
            crate::numeric::decimal::Dec::new(rust_decimal::Decimal::from(-1)),
        ] {
            let ev = event(
                EventKind::Valuation {
                    instrument: InstrumentId::new_random(),
                    price,
                    currency: CurrencyCode::Rub,
                    quality: crate::valuation::PriceQuality::OwnerEstimate,
                },
                vec![],
                acc,
            );
            assert!(matches!(
                ev.validate_structure(),
                Err(EventValidationError::NonPositive { field: "price", .. })
            ));
        }
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

    #[test]
    fn a_control_assertion_carries_no_legs() {
        // Утверждение о полноте интервала не двигает денег. Нога у него
        // означала бы, что контрольная секция отчёта попала в остаток
        // вторым экземпляром и удвоила его.
        use crate::reconciliation::claim::{AssertionPeriod, BalancePoint, ControlClaim};

        let account = AccountId::new_random();
        let period =
            AssertionPeriod::between(date!(2026 - 03 - 01), date!(2026 - 03 - 31)).unwrap();
        let claim = ControlClaim::CashBalance {
            currency: CurrencyCode::Rub,
            amount: PostedMinor::new(1_000_000),
            at: BalancePoint::Closing,
        };
        let kind = EventKind::ControlAssertion { period, claim };

        let clean =
            test_support::event_with(account, date!(2026 - 03 - 31), 1, kind.clone(), vec![]);
        assert!(clean.validate_structure().is_ok());

        let with_leg = test_support::event_with(
            account,
            date!(2026 - 03 - 31),
            2,
            kind,
            vec![Leg::cash(account, rub(1_000_000))],
        );
        assert!(matches!(
            with_leg.validate_structure(),
            Err(EventValidationError::LegCount { .. })
        ));
    }

    #[test]
    fn a_control_assertion_with_an_inverted_period_is_rejected() {
        // Конструктор такой интервал не создаёт, но событие приходит
        // и из JSON, где конструктор не вызывался. Валидация формы —
        // второй рубеж, и он обязан ловить состояние, а не полагаться
        // на то, что первый рубеж отработал.
        use crate::reconciliation::claim::{AssertionPeriod, BalancePoint, ControlClaim};

        assert!(AssertionPeriod::between(date!(2026 - 03 - 31), date!(2026 - 03 - 01)).is_none());

        let inverted = AssertionPeriod {
            from: date!(2026 - 03 - 31),
            to: date!(2026 - 03 - 01),
        };
        let kind = EventKind::ControlAssertion {
            period: inverted,
            claim: ControlClaim::CashBalance {
                currency: CurrencyCode::Rub,
                amount: PostedMinor::new(1),
                at: BalancePoint::Opening,
            },
        };
        let event = test_support::event_with(
            AccountId::new_random(),
            date!(2026 - 03 - 01),
            1,
            kind,
            vec![],
        );
        assert!(matches!(
            event.validate_structure(),
            Err(EventValidationError::NonPositive {
                field: "period",
                ..
            })
        ));
    }

    #[test]
    fn negative_totals_are_rejected_but_a_negative_cash_balance_is_not() {
        // Отрицательный остаток — законное состояние (§11): технический
        // овердрафт и тайминги расчётов. Отрицательная сумма комиссий
        // законным состоянием не является: это ошибка разбора знака,
        // и принять её значит внести её в журнал навсегда.
        use crate::reconciliation::claim::{AssertionPeriod, BalancePoint, ControlClaim};

        let account = AccountId::new_random();
        let period =
            AssertionPeriod::between(date!(2026 - 03 - 01), date!(2026 - 03 - 31)).unwrap();

        let overdraft = EventKind::ControlAssertion {
            period,
            claim: ControlClaim::CashBalance {
                currency: CurrencyCode::Rub,
                amount: PostedMinor::new(-5_000),
                at: BalancePoint::Closing,
            },
        };
        assert!(
            test_support::event_with(account, date!(2026 - 03 - 31), 1, overdraft, vec![])
                .validate_structure()
                .is_ok()
        );

        let negative_fees = EventKind::ControlAssertion {
            period,
            claim: ControlClaim::FeesTotal {
                currency: CurrencyCode::Rub,
                amount: PostedMinor::new(-100),
            },
        };
        assert!(matches!(
            test_support::event_with(account, date!(2026 - 03 - 31), 2, negative_fees, vec![])
                .validate_structure(),
            Err(EventValidationError::NonPositive {
                field: "amount",
                ..
            })
        ));
    }

    #[test]
    fn a_negative_position_quantity_is_outside_the_perimeter() {
        // Шорты вне периметра (§11). Отрицательное количество в контрольной
        // секции означает либо шорт, либо перепутанный знак — принимать
        // нельзя ни то, ни другое.
        use crate::numeric::decimal::Dec;
        use crate::reconciliation::claim::{AssertionPeriod, BalancePoint, ControlClaim};
        use rust_decimal::Decimal;

        let period =
            AssertionPeriod::between(date!(2026 - 03 - 01), date!(2026 - 03 - 31)).unwrap();
        let kind = EventKind::ControlAssertion {
            period,
            claim: ControlClaim::PositionQuantity {
                instrument: InstrumentId::new_random(),
                custody: CustodyId::new_random(),
                quantity: Quantity(Dec::new(Decimal::from(-10))),
                at: BalancePoint::Closing,
            },
        };
        assert!(matches!(
            test_support::event_with(
                AccountId::new_random(),
                date!(2026 - 03 - 31),
                1,
                kind,
                vec![]
            )
            .validate_structure(),
            Err(EventValidationError::NonPositive {
                field: "quantity",
                ..
            })
        ));
    }
}
