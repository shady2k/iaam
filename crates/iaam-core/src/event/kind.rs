//! Семейство типов событий (§4.6).
//!
//! На этапе 1 реализовано подмножество, достаточное для ручного ввода
//! и расчёта XIRR до налога. Остальные варианты добавляются на своих
//! этапах — добавление варианта обязано сломать сборку везде, где
//! разбор не полон.

use serde::{Deserialize, Serialize};

use crate::ids::{AccountId, InstrumentId, TransferId};
use crate::money::{CurrencyCode, Money, Quantity};
use crate::numeric::decimal::Dec;
use crate::reconciliation::claim::{AssertionPeriod, ControlClaim};
use crate::valuation::PriceQuality;

/// Уверенность в количестве (§10.7).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Certainty {
    Known,
    Estimated,
}

/// Уверенность в дате приобретения.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum DateCertainty {
    Known,
    Estimated,
    Unknown,
}

/// Уверенность в налоговой стоимости.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum BasisCertainty {
    Documented,
    Estimated,
    Unknown,
}

/// Троичный ответ. `Unknown` — полноценное значение, а не «нет» (§4.9).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Tristate {
    Yes,
    No,
    Unknown,
}

/// Известно ли что-то вообще.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Knowledge {
    Known,
    Unknown,
}

/// Восстановленное начало как **набор утверждений с уверенностью**
/// (§10.7), а не строка с ценой.
///
/// Умолчание — «неизвестно» по каждому пункту. Это не заглушка: событие,
/// записанное до появления этого поля, действительно ничего из
/// перечисленного не утверждало, и приписать ему `Known` значило бы
/// задним числом объявить документированным то, чего никто не видел.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct OpeningAssertions {
    pub quantity: Certainty,
    pub acquisition_date: Option<time::Date>,
    pub acquisition_date_certainty: DateCertainty,
    pub tax_basis: BasisCertainty,
    pub basis_currency: Option<CurrencyCode>,
    pub basis_rate: Option<Dec>,
    pub fees_included: Tristate,
    pub ldv_eligibility: Knowledge,
    pub prior_corporate_actions: Knowledge,
}

impl Default for OpeningAssertions {
    fn default() -> Self {
        Self {
            // Количество восстановленной позиции — оценка, пока владелец
            // не сказал иного: «известно» по умолчанию означало бы, что
            // система сама подтвердила то, что ей продиктовали.
            quantity: Certainty::Estimated,
            acquisition_date: None,
            acquisition_date_certainty: DateCertainty::Unknown,
            tax_basis: BasisCertainty::Unknown,
            basis_currency: None,
            basis_rate: None,
            fees_included: Tristate::Unknown,
            ldv_eligibility: Knowledge::Unknown,
            prior_corporate_actions: Knowledge::Unknown,
        }
    }
}

impl OpeningAssertions {
    /// Достаточно ли известно, чтобы считать налоговую стоимость.
    ///
    /// Используется отчётом: если стоимость неизвестна, налоговый отчёт
    /// обязан вернуть диапазон или `not_computable`, но не точную цифру
    /// (§10.7). Сам расчёт появится в E5.
    #[must_use]
    pub const fn basis_is_documented(&self) -> bool {
        matches!(self.tax_basis, BasisCertainty::Documented)
    }
}

/// Направление сделки.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum TradeSide {
    Buy,
    Sell,
}

/// Тип события. Исчерпаемый — `#[non_exhaustive]` намеренно **не**
/// применяется: внешних потребителей у ядра нет, а исчерпаемость даёт
/// проверку полноты разбора (§15.1).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum EventKind {
    /// Покупка или продажа.
    Trade {
        side: TradeSide,
        instrument: InstrumentId,
        quantity: Quantity,
        gross: Money,
        fee: Option<Money>,
        /// НКД, уплаченный продавцу или полученный от покупателя (§7.2).
        accrued_interest: Option<Money>,
    },
    /// Деньги вошли в контур извне (§4.10).
    CashIn { amount: Money },
    /// Деньги вышли из контура.
    CashOut { amount: Money },
    /// Движение денег между счетами.
    ///
    /// **Оба счёта хранятся в самом событии.** Классификация относительно
    /// контура невозможна без второго счёта: перевод с внешнего вклада на
    /// внутренний брокерский счёт — внешний поток, а между двумя внутренними
    /// счетами — нет. Событие необратимо, поэтому недостающая семантика
    /// здесь означала бы миграцию журнала позже (§16.1).
    CashTransfer {
        transfer_id: TransferId,
        from: AccountId,
        to: AccountId,
        amount: Money,
    },
    /// Купон, дивиденд, фактически выплаченные проценты.
    Income {
        instrument: Option<InstrumentId>,
        gross: Money,
    },
    /// Комиссия, не привязанная к сделке.
    Fee { amount: Money, origin: FeeOrigin },
    /// Восстановленная позиция для счёта без истории (§10.7).
    OpeningPosition {
        instrument: InstrumentId,
        quantity: Quantity,
        cost_basis: Option<Money>,
        /// Набор утверждений о восстановленном начале (§10.7).
        ///
        /// `#[serde(default)]` обязателен: журнал append-only, и уже
        /// записанные события этого поля не содержат. Отсутствие поля
        /// означает «ничего из этого не утверждалось», а не выдуманные
        /// значения.
        #[serde(default)]
        assertions: OpeningAssertions,
    },
    /// Восстановленный денежный остаток.
    OpeningCash { amount: Money },
    /// Оценка инструмента по цене за единицу (§5.4).
    ///
    /// Факт с provenance, а не расчёт: цену кто-то опубликовал или назвал,
    /// и без неё стоимость позиции неизвестна. На этапе 1 источник —
    /// владелец или внешний агент; в E3 тот же вариант заполняет
    /// `iaam-market`, и схема от этого не меняется.
    ///
    /// Денег не двигает: ног у события нет.
    Valuation {
        instrument: InstrumentId,
        price: Dec,
        currency: CurrencyCode,
        quality: PriceQuality,
    },
    /// Контрольное утверждение источника о полноте интервала (§10.3).
    ///
    /// Факт с provenance, а не расчёт: контрольная секция отчёта — это
    /// то, что источник о себе сказал. Сверка сравнивает её с тем, что
    /// насчитала проекция, и из совпадения рождается основание повышения
    /// статуса. Денег не двигает: ног у события нет.
    ControlAssertion {
        period: AssertionPeriod,
        claim: ControlClaim,
    },
}

/// Происхождение комиссии. Нужно уже на этапе 1, потому что проценты
/// по марже импортируются как комиссия с пометкой (§11).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum FeeOrigin {
    Brokerage,
    Depositary,
    AccountMaintenance,
    /// Проценты по марже. Позиция вне периметра, но денежный эффект сохраняется.
    MarginInterest,
    Other,
}

impl EventKind {
    /// Короткое машиночитаемое имя. Используется в API и хранилище.
    ///
    /// Реализовано исчерпывающим `match` без ветки `_`: добавление
    /// варианта обязано сломать сборку здесь.
    #[must_use]
    pub const fn discriminant(&self) -> &'static str {
        match self {
            Self::Trade { .. } => "trade",
            Self::CashIn { .. } => "cash_in",
            Self::CashOut { .. } => "cash_out",
            Self::CashTransfer { .. } => "cash_transfer",
            Self::Income { .. } => "income",
            Self::Fee { .. } => "fee",
            Self::OpeningPosition { .. } => "opening_position",
            Self::OpeningCash { .. } => "opening_cash",
            Self::Valuation { .. } => "valuation",
            Self::ControlAssertion { .. } => "control_assertion",
        }
    }

    /// Куда и откуда движутся деньги.
    ///
    /// Само по себе событие **не знает**, пересекает ли оно границу контура:
    /// это свойство пары «событие + определение контура». Классификацию
    /// делает классификатор контура (модуль `contour`, следующая задача),
    /// а здесь описываются только конечные точки движения.
    #[must_use]
    pub const fn flow_endpoints(&self) -> FlowEndpoints {
        match self {
            Self::CashIn { .. } => FlowEndpoints::InboundFromOutside,
            Self::CashOut { .. } => FlowEndpoints::OutboundToOutside,
            Self::CashTransfer { from, to, .. } => FlowEndpoints::BetweenAccounts {
                from: *from,
                to: *to,
            },
            Self::Trade { .. }
            | Self::Income { .. }
            | Self::Fee { .. }
            | Self::OpeningPosition { .. }
            | Self::OpeningCash { .. }
            | Self::Valuation { .. }
            | Self::ControlAssertion { .. } => FlowEndpoints::WithinAccount,
        }
    }
}

/// Конечные точки денежного движения события.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FlowEndpoints {
    /// Деньги пришли от контрагента, которого система не наблюдает.
    InboundFromOutside,
    /// Деньги ушли контрагенту, которого система не наблюдает.
    OutboundToOutside,
    /// Движение между двумя известными счетами.
    BetweenAccounts { from: AccountId, to: AccountId },
    /// Движение внутри одного счёта: покупка, купон, комиссия.
    WithinAccount,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::money::{CurrencyCode, PostedMinor};

    fn rub(minor: i64) -> Money {
        Money::new(PostedMinor::new(minor), CurrencyCode::Rub)
    }

    fn trade(side: TradeSide) -> EventKind {
        EventKind::Trade {
            side,
            instrument: InstrumentId::new_random(),
            quantity: Quantity::zero(),
            gross: rub(5_000_000),
            fee: None,
            accrued_interest: None,
        }
    }

    fn transfer() -> EventKind {
        EventKind::CashTransfer {
            transfer_id: TransferId::new_random(),
            from: AccountId::new_random(),
            to: AccountId::new_random(),
            amount: rub(10_000_000),
        }
    }

    fn opening_position() -> EventKind {
        EventKind::OpeningPosition {
            instrument: InstrumentId::new_random(),
            quantity: Quantity::zero(),
            cost_basis: None,
            assertions: OpeningAssertions::default(),
        }
    }

    // --- Дискриминант ---

    #[test]
    fn every_variant_has_its_own_discriminant() {
        // Имена уходят в API и хранилище: их совпадение или подмена
        // означала бы, что два разных факта записаны одинаково.
        assert_eq!(trade(TradeSide::Buy).discriminant(), "trade");
        assert_eq!(
            EventKind::CashIn { amount: rub(1) }.discriminant(),
            "cash_in"
        );
        assert_eq!(
            EventKind::CashOut { amount: rub(1) }.discriminant(),
            "cash_out"
        );
        assert_eq!(transfer().discriminant(), "cash_transfer");
        assert_eq!(
            EventKind::Income {
                instrument: None,
                gross: rub(1)
            }
            .discriminant(),
            "income"
        );
        assert_eq!(
            EventKind::Fee {
                amount: rub(-1),
                origin: FeeOrigin::Brokerage
            }
            .discriminant(),
            "fee"
        );
        assert_eq!(opening_position().discriminant(), "opening_position");
        assert_eq!(
            EventKind::OpeningCash { amount: rub(1) }.discriminant(),
            "opening_cash"
        );
    }

    #[test]
    fn the_side_of_a_trade_does_not_change_its_discriminant() {
        // Покупка и продажа — один тип события с разным направлением,
        // а не два типа: списание лотов различает их по `side`.
        assert_eq!(
            trade(TradeSide::Buy).discriminant(),
            trade(TradeSide::Sell).discriminant()
        );
    }

    // --- Конечные точки движения ---

    #[test]
    fn external_cash_has_outside_endpoints() {
        assert_eq!(
            EventKind::CashIn { amount: rub(1) }.flow_endpoints(),
            FlowEndpoints::InboundFromOutside
        );
        assert_eq!(
            EventKind::CashOut { amount: rub(1) }.flow_endpoints(),
            FlowEndpoints::OutboundToOutside
        );
    }

    #[test]
    fn transfer_reports_both_accounts() {
        let from = AccountId::new_random();
        let to = AccountId::new_random();
        let kind = EventKind::CashTransfer {
            transfer_id: TransferId::new_random(),
            from,
            to,
            amount: rub(10_000_000),
        };
        assert_eq!(
            kind.flow_endpoints(),
            FlowEndpoints::BetweenAccounts { from, to }
        );
    }

    #[test]
    fn transfer_endpoints_keep_their_direction() {
        // Перепутанные местами счета — другое событие: перевод
        // со вклада на брокерский счёт и обратный ему классифицируются
        // контуром по-разному.
        let from = AccountId::new_random();
        let to = AccountId::new_random();
        let kind = EventKind::CashTransfer {
            transfer_id: TransferId::new_random(),
            from,
            to,
            amount: rub(10_000_000),
        };
        assert_ne!(
            kind.flow_endpoints(),
            FlowEndpoints::BetweenAccounts { from: to, to: from }
        );
    }

    #[test]
    fn buying_a_security_stays_within_the_account() {
        let kind = EventKind::Trade {
            side: TradeSide::Buy,
            instrument: InstrumentId::new_random(),
            quantity: Quantity::zero(),
            gross: rub(5_000_000),
            fee: None,
            accrued_interest: None,
        };
        assert_eq!(kind.flow_endpoints(), FlowEndpoints::WithinAccount);
    }

    #[test]
    fn income_stays_within_the_account() {
        assert_eq!(
            EventKind::Income {
                instrument: None,
                gross: rub(100_000)
            }
            .flow_endpoints(),
            FlowEndpoints::WithinAccount
        );
    }

    #[test]
    fn fee_and_opening_balances_stay_within_the_account() {
        // Комиссия и восстановленные остатки не пересекают границу контура
        // сами по себе: восстановленный остаток — не внешний приток денег.
        assert_eq!(
            EventKind::Fee {
                amount: rub(-3_500),
                origin: FeeOrigin::AccountMaintenance
            }
            .flow_endpoints(),
            FlowEndpoints::WithinAccount
        );
        assert_eq!(
            opening_position().flow_endpoints(),
            FlowEndpoints::WithinAccount
        );
        assert_eq!(
            EventKind::OpeningCash {
                amount: rub(10_000_000)
            }
            .flow_endpoints(),
            FlowEndpoints::WithinAccount
        );
    }
}
