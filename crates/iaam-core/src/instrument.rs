//! Справочник инструментов: род, псевдонимы, роли валют (§4.5, §5.4, §7.2).
//!
//! Здесь только неизменные свойства инструмента. Строка политики
//! оценки §5.4 зависит ещё и от наличия цены и её возраста на дату,
//! поэтому выводится функцией в E3.3, а не хранится колонкой.

use serde::{Deserialize, Serialize};
use time::Date;

use crate::ids::InstrumentId;
use crate::money::CurrencyCode;

/// Род инструмента. Неизменное свойство: акция не становится облигацией.
///
/// Исчерпаемый `enum` без `#[non_exhaustive]` по образцу [`CurrencyCode`]:
/// добавление рода обязано сломать сборку везде, где его не обработали
/// (§15.1).
///
/// Вариантов `Futures` и `Option` здесь нет намеренно: §11 выводит ПФИ
/// за периметр вместе с шортами, маржой и РЕПО, и ledger обязательств
/// не строится. Вариант `Deposit` отсутствует по другой причине: вклад
/// является счётом, а не инструментом — у него нет ни количества, ни
/// места хранения (§4.5, и doc-комментарий `AccountId` прямо называет
/// вклад денежным счётом).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub enum InstrumentKind {
    Share,
    DepositaryReceipt,
    Bond,
    /// Биржевой фонд: есть котировка.
    Etf,
    /// Паевой фонд: расчётная стоимость пая, а не котировка.
    MutualFund,
    Currency,
    Crypto,
    RealEstate,
    PrivateShare,
    Loan,
}

impl InstrumentKind {
    /// Все варианты. Существует ради табличных тестов: список,
    /// собранный руками в тесте, разъедется с `enum` молча.
    pub const ALL: [Self; 10] = [
        Self::Share,
        Self::DepositaryReceipt,
        Self::Bond,
        Self::Etf,
        Self::MutualFund,
        Self::Currency,
        Self::Crypto,
        Self::RealEstate,
        Self::PrivateShare,
        Self::Loan,
    ];

    #[must_use]
    pub const fn code(self) -> &'static str {
        match self {
            Self::Share => "share",
            Self::DepositaryReceipt => "depositary_receipt",
            Self::Bond => "bond",
            Self::Etf => "etf",
            Self::MutualFund => "mutual_fund",
            Self::Currency => "currency",
            Self::Crypto => "crypto",
            Self::RealEstate => "real_estate",
            Self::PrivateShare => "private_share",
            Self::Loan => "loan",
        }
    }

    /// Разбор кода. `None`, а не подстановка умолчания: неизвестный род
    /// обязан дойти до вызывающего, а не превратиться в акцию (§4.9).
    #[must_use]
    pub fn from_code(code: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|kind| kind.code() == code)
    }
}

/// Пространство имён внешнего кода инструмента.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub enum AliasNamespace {
    Isin,
    MoexSecid,
    Ticker,
    Figi,
    /// Внутренний код брокера: у разных брокеров разный для одной бумаги.
    BrokerCode,
}

impl AliasNamespace {
    pub const ALL: [Self; 5] = [
        Self::Isin,
        Self::MoexSecid,
        Self::Ticker,
        Self::Figi,
        Self::BrokerCode,
    ];

    #[must_use]
    pub const fn code(self) -> &'static str {
        match self {
            Self::Isin => "isin",
            Self::MoexSecid => "moex_secid",
            Self::Ticker => "ticker",
            Self::Figi => "figi",
            Self::BrokerCode => "broker_code",
        }
    }

    #[must_use]
    pub fn from_code(code: &str) -> Option<Self> {
        Self::ALL
            .into_iter()
            .find(|namespace| namespace.code() == code)
    }
}

/// Почему у инструмента есть предшественник (§7.2, §4.7).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub enum LineageReason {
    /// Замещающая облигация.
    Replacement,
    Conversion,
    Merger,
    SpinOff,
}

impl LineageReason {
    pub const ALL: [Self; 4] = [
        Self::Replacement,
        Self::Conversion,
        Self::Merger,
        Self::SpinOff,
    ];

    #[must_use]
    pub const fn code(self) -> &'static str {
        match self {
            Self::Replacement => "replacement",
            Self::Conversion => "conversion",
            Self::Merger => "merger",
            Self::SpinOff => "spin_off",
        }
    }

    #[must_use]
    pub fn from_code(code: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|reason| reason.code() == code)
    }
}

/// Три роли валюты у одного инструмента (§7.2).
///
/// Структура, а не три позиционных `CurrencyCode`: одинаково
/// типизированные аргументы подряд переставляются местами незаметно
/// для компилятора (§15.1). Валюты отчёта здесь нет — она свойство
/// отчёта и владельца, а не бумаги.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct CurrencyRoles {
    /// Валюта обязательства.
    pub denomination: CurrencyCode,
    /// Валюта расчётов.
    pub settlement: CurrencyCode,
    /// Валюта котировки.
    pub quote: CurrencyCode,
}

impl CurrencyRoles {
    /// Все три роли совпадают — обычный случай рублёвой бумаги.
    #[must_use]
    pub const fn uniform(currency: CurrencyCode) -> Self {
        Self {
            denomination: currency,
            settlement: currency,
            quote: currency,
        }
    }
}

/// Интервал действия псевдонима.
///
/// Начало включительно, конец исключительно. Полуинтервал выбран,
/// чтобы смежные интервалы одного кода стыковались без зазора и без
/// перекрытия: при включительном конце день смены ISIN принадлежал бы
/// сразу двум записям.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct AliasInterval {
    pub valid_from: Date,
    /// `None` — открытый интервал.
    pub valid_to: Option<Date>,
}

impl AliasInterval {
    #[must_use]
    pub fn covers(&self, on: Date) -> bool {
        on >= self.valid_from && self.valid_to.is_none_or(|end| on < end)
    }
}

/// Происхождение инструмента: замещение, конвертация, слияние (§7.2).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Lineage {
    pub parent: InstrumentId,
    pub reason: LineageReason,
}
#[cfg(test)]
mod tests {
    use super::*;
    use time::macros::date;

    #[test]
    fn an_interval_includes_its_first_day() {
        let interval = AliasInterval {
            valid_from: date!(2023 - 01 - 10),
            valid_to: None,
        };
        assert!(interval.covers(date!(2023 - 01 - 10)));
    }

    #[test]
    fn an_interval_excludes_the_day_it_ends() {
        let interval = AliasInterval {
            valid_from: date!(2023 - 01 - 10),
            valid_to: Some(date!(2024 - 05 - 20)),
        };
        assert!(interval.covers(date!(2024 - 05 - 19)));
        assert!(!interval.covers(date!(2024 - 05 - 20)));
    }

    #[test]
    fn an_open_interval_covers_every_later_day() {
        let interval = AliasInterval {
            valid_from: date!(2023 - 01 - 10),
            valid_to: None,
        };
        assert!(interval.covers(date!(2099 - 12 - 31)));
        assert!(!interval.covers(date!(2023 - 01 - 09)));
    }

    #[test]
    fn every_kind_survives_a_round_trip_through_its_code() {
        for kind in InstrumentKind::ALL {
            assert_eq!(InstrumentKind::from_code(kind.code()), Some(kind));
        }
    }

    #[test]
    fn every_namespace_survives_a_round_trip_through_its_code() {
        for namespace in AliasNamespace::ALL {
            assert_eq!(AliasNamespace::from_code(namespace.code()), Some(namespace));
        }
    }

    #[test]
    fn every_lineage_reason_survives_a_round_trip_through_its_code() {
        for reason in LineageReason::ALL {
            assert_eq!(LineageReason::from_code(reason.code()), Some(reason));
        }
    }

    #[test]
    fn an_unknown_code_is_not_guessed() {
        assert_eq!(InstrumentKind::from_code("derivative"), None);
        assert_eq!(AliasNamespace::from_code("cusip"), None);
    }
}
