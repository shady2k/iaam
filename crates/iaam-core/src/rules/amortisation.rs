//! Распределение возвращённой стоимости при амортизации (§6.5).
//!
//! Отдельный трейт и отдельная версия, а не расширение
//! [`super::lot_disposal::LotDisposalRule`]: списание лотов — выбор
//! владельца (FIFO против прочих), амортизация — событие выпуска.
//! Общий номер версии связал бы два независимых решения, и смена метода
//! списания задним числом переписала бы историю амортизаций.

use serde::{Deserialize, Serialize};
use thiserror::Error;

use super::lot_disposal::{DisposalError, Lot, PrincipalState, split_basis};
use crate::money::{Money, PerUnitAmount};

/// Версия правила амортизации. Своя, не общая со списанием лотов.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct AmortisationRuleVersion(pub u32);

#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum AmortisationError {
    #[error("номинал лота неизвестен: долю возврата считать не от чего")]
    UnknownPrincipal,
    #[error("номинал, возврат и стоимость лота не в одной валюте")]
    CurrencyMismatch,
    #[error("возвращено больше номинала, чем осталось непогашенным")]
    ReturnedAboveRemaining,
    #[error("возврат номинала не положителен: событие ничего не вернуло")]
    ReturnedNotPositive,
    #[error(transparent)]
    Disposal(#[from] DisposalError),
}

/// Сколько налоговой стоимости лота возвращается вместе с номиналом.
pub trait AmortisationRule: Send + Sync + std::fmt::Debug {
    fn basis_returned(
        &self,
        lot: &Lot,
        returned_per_unit: PerUnitAmount,
    ) -> Result<Money, AmortisationError>;
}

/// Доля стоимости, пропорциональная доле возвращённого номинала.
#[derive(Debug, Default)]
pub struct ProRataV1;

impl AmortisationRule for ProRataV1 {
    fn basis_returned(
        &self,
        lot: &Lot,
        returned_per_unit: PerUnitAmount,
    ) -> Result<Money, AmortisationError> {
        let PrincipalState::Known {
            remaining_per_unit, ..
        } = lot.principal
        else {
            // Ноль означал бы «амортизация ничего не вернула» — неправда,
            // а не отсутствие данных (§4.9).
            return Err(AmortisationError::UnknownPrincipal);
        };
        if remaining_per_unit.currency() != returned_per_unit.currency()
            || remaining_per_unit.currency() != lot.cost_basis.currency()
        {
            // Пересчёт по случайному курсу хуже отказа.
            return Err(AmortisationError::CurrencyMismatch);
        }
        if !returned_per_unit.value().is_positive() {
            return Err(AmortisationError::ReturnedNotPositive);
        }
        if returned_per_unit.value() > remaining_per_unit.value() {
            // Заодно закрывает деление на ноль: положительный возврат
            // при нулевом остатке сюда и попадает.
            return Err(AmortisationError::ReturnedAboveRemaining);
        }
        // Знаменатель — номинал **до** события. Округление и обрезка
        // живут в `split_basis`: она решает ровно эту задачу «доля от
        // суммы» с конвенцией «половина к чётному», и своя конвенция
        // внутри одного ядра означала бы два разных ответа на один вопрос.
        Ok(split_basis(
            lot.cost_basis,
            returned_per_unit.value().inner(),
            remaining_per_unit.value().inner(),
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
    use crate::rules::lot_disposal::LotId;
    use rust_decimal::Decimal;
    use time::macros::date;

    fn rub(minor: i64) -> Money {
        Money::new(PostedMinor::new(minor), CurrencyCode::Rub)
    }

    fn dec(text: &str) -> Dec {
        Dec::new(Decimal::from_str_exact(text).unwrap())
    }

    fn per_unit(text: &str) -> PerUnitAmount {
        PerUnitAmount::new(dec(text), CurrencyCode::Rub)
    }

    fn usd_per_unit(text: &str) -> PerUnitAmount {
        PerUnitAmount::new(dec(text), CurrencyCode::Usd)
    }

    fn lot(cost_basis: Money, principal: PrincipalState) -> Lot {
        Lot {
            id: LotId::new_random(),
            instrument: InstrumentId::new_random(),
            acquired: Some(TradeDate(date!(2026 - 01 - 10))),
            quantity: Quantity(dec("100")),
            cost_basis,
            principal,
        }
    }

    fn known(original: &str, remaining: &str) -> PrincipalState {
        PrincipalState::known(per_unit(original), per_unit(remaining)).unwrap()
    }

    #[test]
    fn basis_returned_is_proportional_to_the_principal_before_the_event() {
        // Номинал 1000, возвращено 200 — пятая часть стоимости 100 000.
        let lot = lot(rub(100_000), known("1000", "1000"));
        assert_eq!(
            ProRataV1.basis_returned(&lot, per_unit("200")).unwrap(),
            rub(20_000)
        );
    }

    #[test]
    fn a_lot_bought_between_amortisations_uses_its_own_remaining_principal() {
        // Лот куплен уже амортизированным: доля считается от 800, а не
        // от 1000, — иначе владелец второго лота вернул бы себе стоимость,
        // которой не платил.
        let lot = lot(rub(100_000), known("1000", "800"));
        assert_eq!(
            ProRataV1.basis_returned(&lot, per_unit("200")).unwrap(),
            rub(25_000)
        );
    }

    #[test]
    fn an_unknown_principal_refuses_instead_of_guessing() {
        let lot = lot(rub(100_000), PrincipalState::Unknown);
        assert_eq!(
            ProRataV1.basis_returned(&lot, per_unit("200")).unwrap_err(),
            AmortisationError::UnknownPrincipal
        );
    }

    #[test]
    fn a_nominal_currency_other_than_the_returned_currency_refuses() {
        let lot = lot(rub(100_000), known("1000", "1000"));
        assert_eq!(
            ProRataV1
                .basis_returned(&lot, usd_per_unit("200"))
                .unwrap_err(),
            AmortisationError::CurrencyMismatch
        );
    }

    #[test]
    fn a_nominal_currency_other_than_the_basis_currency_refuses() {
        // Пересчёт по случайному курсу хуже отказа.
        let usd_nominal =
            PrincipalState::known(usd_per_unit("1000"), usd_per_unit("1000")).unwrap();
        let lot = lot(rub(100_000), usd_nominal);
        assert_eq!(
            ProRataV1
                .basis_returned(&lot, usd_per_unit("200"))
                .unwrap_err(),
            AmortisationError::CurrencyMismatch
        );
    }

    #[test]
    fn rounding_follows_the_same_convention_as_lot_disposal() {
        // Половина копейки уходит к чётному — как в split_basis.
        let lot = lot(rub(5), known("2", "2"));
        assert_eq!(
            ProRataV1.basis_returned(&lot, per_unit("1")).unwrap(),
            rub(2)
        );
    }

    #[test]
    fn returning_more_principal_than_remains_refuses() {
        // Вернуть больше, чем осталось, невозможно: факт противоречит лоту.
        let lot = lot(rub(100_000), known("1000", "200"));
        assert_eq!(
            ProRataV1.basis_returned(&lot, per_unit("201")).unwrap_err(),
            AmortisationError::ReturnedAboveRemaining
        );
    }

    #[test]
    fn returning_nothing_is_not_an_amortisation() {
        // Ноль в знаменателе — деление на ноль, а ноль в числителе
        // означает событие, которое ничего не вернуло.
        let lot = lot(rub(100_000), known("1000", "1000"));
        assert_eq!(
            ProRataV1.basis_returned(&lot, per_unit("0")).unwrap_err(),
            AmortisationError::ReturnedNotPositive
        );
        assert_eq!(
            ProRataV1.basis_returned(&lot, per_unit("-1")).unwrap_err(),
            AmortisationError::ReturnedNotPositive
        );
    }

    #[test]
    fn the_whole_basis_comes_back_when_the_whole_principal_does() {
        // Погашение целиком: остаток стоимости не должен зависнуть в лоте.
        let lot = lot(rub(100_000), known("1000", "1000"));
        assert_eq!(
            ProRataV1.basis_returned(&lot, per_unit("1000")).unwrap(),
            rub(100_000)
        );
    }

    #[test]
    fn the_registry_resolves_the_default_amortisation_rule() {
        let registry = crate::rules::RuleRegistry::with_defaults();
        let version = registry
            .latest_amortisation_version()
            .expect("реестр не пуст по умолчанию");
        assert_eq!(version, AmortisationRuleVersion(1));
        let lot = lot(rub(100_000), known("1000", "1000"));
        assert_eq!(
            registry
                .amortisation_rule(version)
                .expect("версия разрешается")
                .basis_returned(&lot, per_unit("200"))
                .unwrap(),
            rub(20_000)
        );
    }

    #[test]
    fn an_unknown_amortisation_version_does_not_fall_back_to_another_rule() {
        let registry = crate::rules::RuleRegistry::with_defaults();
        assert!(
            registry
                .amortisation_rule(AmortisationRuleVersion(999))
                .is_none()
        );
    }
}
