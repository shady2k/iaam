//! Доменные права и условия окон оферты.

use serde::{Deserialize, Serialize};
use thiserror::Error;
use time::Date;
use uuid::Uuid;

pub use crate::event::offer::OfferWindowId;
use crate::ids::InstrumentId;
use crate::numeric::decimal::Dec;

const OFFER_WINDOW_NAMESPACE: Uuid = Uuid::from_u128(0x6d8f_7dc6_9f8a_4d9b_8e3e_5b3a_8f9d_2c11);

/// Право, которое описывает строка окна в справочном графике.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum OfferRight {
    HolderPut,
    HolderPutSettled,
    IssuerCall,
    Other,
}

impl OfferRight {
    /// Перевести доменное значение словаря `offer_kind` в закрытый тип.
    ///
    /// Неизвестное значение — отказ: `Other` доступен только если словарь
    /// явно сообщил именно это значение.
    pub fn from_dictionary_meaning(meaning: &str) -> Result<Self, OfferWindowError> {
        match meaning {
            "put_option" => Ok(Self::HolderPut),
            "put_option_settled" => Ok(Self::HolderPutSettled),
            "issuer_call" => Ok(Self::IssuerCall),
            "other" => Ok(Self::Other),
            unknown => Err(OfferWindowError::UnknownRight {
                meaning: unknown.to_owned(),
            }),
        }
    }
}

/// Типизированные условия одного окна оферты.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OfferWindowTerms {
    pub window: OfferWindowId,
    pub right: OfferRight,
    pub execution_date: Date,
    pub submission_start: Option<Date>,
    pub submission_end: Option<Date>,
    pub price_percent: Option<Dec>,
}

impl OfferWindowId {
    /// Вывести устойчивый идентификатор окна из выпуска и даты исполнения.
    ///
    /// Свободная формулировка вида источника намеренно не участвует в имени.
    #[must_use]
    pub fn derive(instrument: InstrumentId, execution_date: Date) -> Self {
        let mut name = Vec::with_capacity(16 + 1 + 10);
        name.extend_from_slice(instrument.inner().as_bytes());
        name.push(b'|');
        name.extend_from_slice(execution_date.to_string().as_bytes());
        Self(Uuid::new_v5(&OFFER_WINDOW_NAMESPACE, &name))
    }
}

/// Ошибка структурного перевода окон графика.
#[derive(Debug, Error, Clone, PartialEq, Eq)]
pub enum OfferWindowError {
    #[error("несколько окон оферты имеют дату исполнения {execution_date}")]
    AmbiguousWindow { execution_date: Date },
    #[error("неизвестное доменное значение права оферты: {meaning}")]
    UnknownRight { meaning: String },
}

/// Проверить, что в снимке нет двух неразличимых окон.
pub fn validate_unique_windows(windows: &[OfferWindowTerms]) -> Result<(), OfferWindowError> {
    let mut dates = std::collections::BTreeSet::new();
    for window in windows {
        if !dates.insert(window.execution_date) {
            return Err(OfferWindowError::AmbiguousWindow {
                execution_date: window.execution_date,
            });
        }
    }
    Ok(())
}

/// Вердикт уже выполненной проверки полноты источника.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum ScheduleCompleteness {
    Validated,
    Incomplete { reason: String },
    Unknown,
}

impl Default for ScheduleCompleteness {
    fn default() -> Self {
        Self::Unknown
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use time::macros::date;

    fn terms(date: Date) -> OfferWindowTerms {
        OfferWindowTerms {
            window: OfferWindowId::derive(InstrumentId::new_random(), date),
            right: OfferRight::HolderPut,
            execution_date: date,
            submission_start: None,
            submission_end: None,
            price_percent: None,
        }
    }

    #[test]
    fn derived_window_id_ignores_source_wording() {
        let instrument = InstrumentId::new_random();
        let execution_date = date!(2026 - 12 - 01);
        assert_eq!(
            OfferWindowId::derive(instrument, execution_date),
            OfferWindowId::derive(instrument, execution_date)
        );
    }

    #[test]
    fn derived_window_id_has_a_stable_uuidv5_value() {
        let instrument = InstrumentId(Uuid::parse_str("01234567-89ab-cdef-0123-456789abcdef").unwrap());
        assert_eq!(
            OfferWindowId::derive(instrument, date!(2026 - 12 - 01)).inner(),
            Uuid::parse_str("195df143-b0b6-5e49-bd46-e634d88f31a8").unwrap()
        );
    }


    #[test]
    fn derived_window_id_accepts_years_outside_four_digits() {
        let instrument = InstrumentId::new_random();
        let _ = OfferWindowId::derive(instrument, date!(-1000 - 01 - 01));
    }
    #[test]
    fn duplicate_execution_dates_are_ambiguous() {
        let duplicate = terms(date!(2026 - 12 - 01));
        let error = validate_unique_windows(&[duplicate.clone(), duplicate]).unwrap_err();
        assert_eq!(
            error,
            OfferWindowError::AmbiguousWindow {
                execution_date: date!(2026 - 12 - 01)
            }
        );
    }

    #[test]
    fn known_dictionary_meanings_become_their_typed_rights() {
        assert_eq!(
            OfferRight::from_dictionary_meaning("put_option").unwrap(),
            OfferRight::HolderPut
        );
        assert_eq!(
            OfferRight::from_dictionary_meaning("put_option_settled").unwrap(),
            OfferRight::HolderPutSettled
        );
    }

    #[test]
    fn unknown_dictionary_meaning_is_rejected() {
        assert!(matches!(
            OfferRight::from_dictionary_meaning("new_source_word"),
            Err(OfferWindowError::UnknownRight { .. })
        ));
    }
}
