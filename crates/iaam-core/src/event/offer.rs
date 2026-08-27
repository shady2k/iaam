//! Исполнение оферты (§4.7).
//!
//! Оферта **не** корпоративное действие: это право владельца, а не
//! решение эмитента. Свести выкуп к погашению значило бы потерять
//! и происхождение выбытия, и то, что владелец мог не предъявлять
//! бумагу вовсе — а сценарий «предъявил или додержал» и есть то, ради
//! чего оферту отслеживают.

use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::ids::{CustodyId, InstrumentId};
use crate::money::{Money, Quantity};

/// Поданная заявка на предъявление бумаги к оферте.
///
/// Собственная идентичность, а не идентификатор события: одна заявка
/// связывает цепочку из нескольких фактов — подачу, отзыв и один или
/// несколько расчётов.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct OfferSubmissionId(pub Uuid);

impl OfferSubmissionId {
    #[must_use]
    pub fn new_random() -> Self {
        Self(Uuid::new_v4())
    }

    #[must_use]
    pub const fn inner(&self) -> Uuid {
        self.0
    }
}

/// Окно приёма заявок по оферте.
///
/// В части 1 — непрозрачная идентичность: реестра окон нет, и проверка
/// «окно существует, заявка подана в срок» отложена в E3.4.6 **явно**.
/// Записать идентичность сейчас дешевле, чем восстанавливать связь потом.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct OfferWindowId(pub Uuid);

impl OfferWindowId {
    #[must_use]
    pub fn new_random() -> Self {
        Self(Uuid::new_v4())
    }

    #[must_use]
    pub const fn inner(&self) -> Uuid {
        self.0
    }
}

/// Что произошло с заявкой по оферте.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum OfferExerciseAction {
    /// Поданная заявка. Ног не имеет: ни денег, ни бумаг она не двигает —
    /// как `ControlAssertion`. Отсутствие ног — тоже форма, и она
    /// проверяется наравне с остальными.
    Submitted {
        submission: OfferSubmissionId,
        window: OfferWindowId,
        instrument: InstrumentId,
        quantity: Quantity,
    },
    /// Отзыв заявки целиком или частично.
    ///
    /// Третий член, а не отсутствие расчёта: §3.5 называет отзыв наряду
    /// с частичным исполнением, и без него незакрытая заявка висела бы
    /// вечно, искажая ожидаемое выбытие.
    Cancelled {
        submission: OfferSubmissionId,
        quantity: Quantity,
    },
    /// Совершённый выкуп. Ноги — `Cash` и отрицательная
    /// `SecurityQuantity`; ноги `Principal` нет: бумага выбывает,
    /// а не возвращает номинал. Расчётов по одной заявке бывает
    /// несколько.
    Settled {
        submission: OfferSubmissionId,
        instrument: InstrumentId,
        custody: CustodyId,
        quantity: Quantity,
        gross: Money,
        fee: Option<Money>,
        accrued_interest: Option<Money>,
    },
}

impl OfferExerciseAction {
    /// Имя члена для диагностики и заслонов.
    #[must_use]
    pub const fn discriminant(&self) -> &'static str {
        match self {
            Self::Submitted { .. } => "offer_submitted",
            Self::Cancelled { .. } => "offer_cancelled",
            Self::Settled { .. } => "offer_settled",
        }
    }

    /// Заявка, к которой относится факт: связь цепочки, доступная без
    /// разбора семейства.
    #[must_use]
    pub const fn submission(&self) -> OfferSubmissionId {
        match self {
            Self::Submitted { submission, .. }
            | Self::Cancelled { submission, .. }
            | Self::Settled { submission, .. } => *submission,
        }
    }

    /// Количество бумаг, которого касается факт.
    #[must_use]
    pub const fn quantity(&self) -> Quantity {
        match self {
            Self::Submitted { quantity, .. }
            | Self::Cancelled { quantity, .. }
            | Self::Settled { quantity, .. } => *quantity,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ids::{CustodyId, InstrumentId};
    use crate::money::{CurrencyCode, PostedMinor};
    use crate::numeric::decimal::Dec;
    use rust_decimal::Decimal;

    fn rub(minor: i64) -> Money {
        Money::new(PostedMinor::new(minor), CurrencyCode::Rub)
    }

    fn qty(text: &str) -> Quantity {
        Quantity(Dec::new(Decimal::from_str_exact(text).unwrap()))
    }

    fn sample_submitted() -> OfferExerciseAction {
        OfferExerciseAction::Submitted {
            submission: OfferSubmissionId::new_random(),
            window: OfferWindowId::new_random(),
            instrument: InstrumentId::new_random(),
            quantity: qty("10"),
        }
    }

    fn sample_cancelled() -> OfferExerciseAction {
        OfferExerciseAction::Cancelled {
            submission: OfferSubmissionId::new_random(),
            quantity: qty("4"),
        }
    }

    fn sample_settled() -> OfferExerciseAction {
        OfferExerciseAction::Settled {
            submission: OfferSubmissionId::new_random(),
            instrument: InstrumentId::new_random(),
            custody: CustodyId::new_random(),
            quantity: qty("6"),
            gross: rub(6_000_000),
            fee: Some(rub(-1_000)),
            accrued_interest: Some(rub(12_345)),
        }
    }

    #[test]
    fn every_offer_action_survives_a_json_round_trip() {
        for action in [sample_submitted(), sample_cancelled(), sample_settled()] {
            let text = serde_json::to_string(&action).unwrap();
            assert_eq!(
                serde_json::from_str::<OfferExerciseAction>(&text).unwrap(),
                action
            );
        }
    }

    #[test]
    fn every_offer_action_names_itself() {
        assert_eq!(sample_submitted().discriminant(), "offer_submitted");
        assert_eq!(sample_cancelled().discriminant(), "offer_cancelled");
        assert_eq!(sample_settled().discriminant(), "offer_settled");
    }

    #[test]
    fn every_offer_action_names_the_submission_it_belongs_to() {
        // Цепочка «подал — отозвал — рассчитались» связывается заявкой,
        // и связь обязана доставаться без разбора семейства.
        let submission = OfferSubmissionId::new_random();
        let action = OfferExerciseAction::Cancelled {
            submission,
            quantity: qty("1"),
        };
        assert_eq!(action.submission(), submission);
    }

    #[test]
    fn the_third_member_is_cancellation_not_an_afterthought() {
        // §3.5 называет отзыв наряду с частичным исполнением: без него
        // незакрытая заявка висела бы вечно и искажала бы ожидаемое
        // выбытие.
        assert!(matches!(
            sample_cancelled(),
            OfferExerciseAction::Cancelled { .. }
        ));
    }
}
