//! Распределение возвращённой стоимости при амортизации (§6.5).
//!
//! Отдельный трейт и отдельная версия, а не расширение
//! [`super::lot_disposal::LotDisposalRule`]: списание лотов — выбор
//! владельца (FIFO против прочих), амортизация — событие выпуска.
//! Общий номер версии связал бы два независимых решения, и смена метода
//! списания задним числом переписала бы историю амортизаций.

use serde::{Deserialize, Serialize};
use thiserror::Error;

use super::lot_disposal::{DisposalError, Lot, split_basis};
use crate::money::Money;
use crate::numeric::NumericError;
use crate::rules::ReturnedShare;

/// Версия правила амортизации. Своя, не общая со списанием лотов.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct AmortisationRuleVersion(pub u32);

#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum AmortisationError {
    #[error(transparent)]
    Numeric(#[from] NumericError),
    #[error(transparent)]
    Disposal(#[from] DisposalError),
}

/// Сколько налоговой стоимости лота возвращается вместе с номиналом.
pub trait AmortisationRule: Send + Sync + std::fmt::Debug {
    /// Аргумент безразмерный: суммы в формуле сокращаются, и правилу
    /// незачем знать ни первоначальный номинал, ни остаток. Долю уже
    /// вычислило приложение и сохранило в самом факте.
    fn basis_returned(&self, lot: &Lot, share: ReturnedShare) -> Result<Money, AmortisationError>;
}

/// Доля стоимости, пропорциональная доле возвращённого номинала.
#[derive(Debug, Default)]
pub struct ProRataV1;

impl AmortisationRule for ProRataV1 {
    fn basis_returned(&self, lot: &Lot, share: ReturnedShare) -> Result<Money, AmortisationError> {
        // Округление и обрезка живут в `split_basis`: она решает ровно
        // задачу «доля от суммы» с конвенцией «половина к чётному»,
        // и своя конвенция внутри одного ядра означала бы два разных
        // ответа на один вопрос. Знаменатель — единица: доля уже взята
        // от остатка до события.
        Ok(split_basis(
            lot.cost_basis,
            share.inner().inner(),
            rust_decimal::Decimal::ONE,
        )?)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dates::TradeDate;
    use crate::ids::InstrumentId;
    use crate::money::{CurrencyCode, PostedMinor, Quantity};
    use crate::numeric::decimal::Dec;
    use crate::rules::ReturnedShare;
    use crate::rules::lot_disposal::LotId;
    use crate::rules::lot_disposal::PrincipalState;
    use rust_decimal::Decimal;
    use time::macros::date;

    fn rub(minor: i64) -> Money {
        Money::new(PostedMinor::new(minor), CurrencyCode::Rub)
    }

    fn dec(text: &str) -> Dec {
        Dec::new(Decimal::from_str_exact(text).unwrap())
    }

    fn share(text: &str) -> ReturnedShare {
        ReturnedShare::new(dec(text)).expect("доля в пределах инварианта")
    }

    fn lot(cost_basis: Money) -> Lot {
        Lot {
            id: LotId::new_random(),
            instrument: InstrumentId::new_random(),
            acquired: Some(TradeDate(date!(2026 - 01 - 10))),
            quantity: Quantity(dec("100")),
            cost_basis,
            acquisition_basis: None,
            accrued_interest_paid: None,
            received_to_date: None,
            principal: PrincipalState::Unknown,
        }
    }

    #[test]
    fn a_fifth_of_the_remaining_principal_returns_a_fifth_of_the_basis() {
        let lot = lot(rub(100_000));
        assert_eq!(
            ProRataV1.basis_returned(&lot, share("0.2")).unwrap(),
            rub(20_000)
        );
    }

    #[test]
    fn the_whole_basis_comes_back_when_the_whole_remainder_does() {
        // Последняя амортизация возвращает весь остаток номинала.
        // Бумага при этом остаётся в позиции: её выбытие — отдельный
        // факт, а не следствие возврата денег.
        let lot = lot(rub(100_000));
        assert_eq!(
            ProRataV1.basis_returned(&lot, share("1")).unwrap(),
            rub(100_000)
        );
    }

    #[test]
    fn rounding_follows_the_half_to_even_convention_of_split_basis() {
        // 101 копейка пополам — 50, а не 51: конвенция «половина
        // к чётному» живёт в `split_basis` и остаётся единственной
        // в ядре.
        let lot = lot(rub(101));
        assert_eq!(
            ProRataV1.basis_returned(&lot, share("0.5")).unwrap(),
            rub(50)
        );
    }
}
