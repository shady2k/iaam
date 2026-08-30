//! Разнесение налоговой стоимости при амортизации как факт события.
//!
//! Доля хранится в самом факте, а не выводится позже: если справочник
//! исправят, вывести её будет неоткуда. Тот же довод, по которому
//! `Conversion` хранит `basis_transfer` — условия живут в решении
//! эмитента, а не в справочнике.

use serde::{Deserialize, Serialize};
use thiserror::Error;
use time::OffsetDateTime;

use crate::rules::ReturnedShare;

/// Почему доля разнесения не вычислена.
///
/// Проекции достаточно одного «неизвестно», но владельцу нужно знать,
/// что именно дозагрузить, а аудиту — что именно разошлось.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AllocationGap {
    /// Событие записано до появления поля либо обогащение не выполнялось.
    NotComputed,
    /// Графика выпуска нет вовсе.
    ScheduleMissing,
    /// График есть, но не проверен.
    ScheduleNotValidated,
    /// В графике нет возврата на дату события.
    NoRepaymentOnDate,
    /// Сумма события не сошлась с плановой долей.
    AmountMismatch,
    /// Валюта возврата не совпала с валютой номинала.
    CurrencyMismatch,
    /// На дату приходится несколько возвратов, которые не удалось
    /// сопоставить событиям.
    AmbiguousSameDateRepayments,
    /// Доли возвратов до даты дают больше 100%.
    InvalidPrefix,
}

impl AllocationGap {
    /// Все варианты. Заслон от забытого кода у нового члена семейства:
    /// тест обходит этот массив, а компилятор ловит несовпадение длины.
    pub const ALL: [Self; 8] = [
        Self::NotComputed,
        Self::ScheduleMissing,
        Self::ScheduleNotValidated,
        Self::NoRepaymentOnDate,
        Self::AmountMismatch,
        Self::CurrencyMismatch,
        Self::AmbiguousSameDateRepayments,
        Self::InvalidPrefix,
    ];

    #[must_use]
    pub const fn code(self) -> &'static str {
        match self {
            Self::NotComputed => "not_computed",
            Self::ScheduleMissing => "schedule_missing",
            Self::ScheduleNotValidated => "schedule_not_validated",
            Self::NoRepaymentOnDate => "no_repayment_on_date",
            Self::AmountMismatch => "amount_mismatch",
            Self::CurrencyMismatch => "currency_mismatch",
            Self::AmbiguousSameDateRepayments => "ambiguous_same_date_repayments",
            Self::InvalidPrefix => "invalid_prefix",
        }
    }
}

/// Версия алгоритма вычисления доли.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct AllocationAlgorithmVersion(pub u16);

/// Отпечаток канонической выборки справочных входов вычисления.
///
/// Покрывает то, от чего зависит доля: номинал с валютой, возвраты,
/// вошедшие в остаток до события, возвраты на дату события, идентичность
/// снимка источника и версию правила группировки одинаковых дат.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct AllocationInputsHash(String);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
#[error("отпечаток входов не является 64 шестнадцатеричными знаками")]
pub struct AllocationInputsHashError;

impl AllocationInputsHash {
    pub fn new(value: impl Into<String>) -> Result<Self, AllocationInputsHashError> {
        let value = value.into();
        if value.len() != 64 || !value.chars().all(|c| c.is_ascii_hexdigit()) {
            return Err(AllocationInputsHashError);
        }
        Ok(Self(value))
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// Из каких дополнительных входов приложение вывело долю.
///
/// Отдельно от `Provenance`: тот отвечает на вопрос «откуда пришёл сырой
/// факт», а это — «из чего выведено производное поле».
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AllocationEvidence {
    pub inputs_hash: AllocationInputsHash,
    pub knowledge_as_of: OffsetDateTime,
    pub algorithm_version: AllocationAlgorithmVersion,
}

/// Доля разнесения с доказательством её вычисления.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum BasisAllocation {
    Unknown(AllocationGap),
    Known {
        share: ReturnedShare,
        evidence: AllocationEvidence,
    },
}

impl Default for BasisAllocation {
    /// Умолчание честное: событие, записанное до появления поля,
    /// действительно ничего не утверждало, и приписать ему долю значило
    /// бы объявить вычисленным то, чего никто не вычислял.
    fn default() -> Self {
        Self::Unknown(AllocationGap::NotComputed)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::numeric::decimal::Dec;
    use crate::rules::ReturnedShare;
    use rust_decimal::Decimal;
    use time::OffsetDateTime;

    fn known() -> BasisAllocation {
        BasisAllocation::Known {
            share: ReturnedShare::new(Dec::new(Decimal::new(2, 1))).expect("доля 0.2"),
            evidence: AllocationEvidence {
                inputs_hash: AllocationInputsHash::new("a".repeat(64)).expect("hex"),
                knowledge_as_of: OffsetDateTime::UNIX_EPOCH,
                algorithm_version: AllocationAlgorithmVersion(1),
            },
        }
    }

    #[test]
    fn the_default_allocation_is_unknown_because_the_field_was_never_filled() {
        assert_eq!(
            BasisAllocation::default(),
            BasisAllocation::Unknown(AllocationGap::NotComputed)
        );
    }

    #[test]
    fn a_known_allocation_survives_a_json_round_trip() {
        let text = serde_json::to_string(&known()).expect("запись");
        assert_eq!(
            serde_json::from_str::<BasisAllocation>(&text).expect("чтение"),
            known()
        );
    }

    #[test]
    fn a_known_allocation_survives_a_cbor_round_trip() {
        let mut body = Vec::new();
        ciborium::into_writer(&known(), &mut body).expect("запись");
        assert_eq!(
            ciborium::from_reader::<BasisAllocation, _>(body.as_slice()).expect("чтение"),
            known()
        );
    }

    #[test]
    fn every_gap_names_its_reason() {
        for gap in AllocationGap::ALL {
            assert!(!gap.code().is_empty());
        }
    }

    #[test]
    fn a_hash_that_is_not_sixty_four_hex_digits_is_rejected() {
        assert!(AllocationInputsHash::new("abc").is_err());
        assert!(AllocationInputsHash::new("z".repeat(64)).is_err());
        assert!(AllocationInputsHash::new("A".repeat(64)).is_ok());
    }
}
