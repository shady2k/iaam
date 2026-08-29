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
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum ScheduleCompleteness {
    Validated,
    Incomplete {
        reason: String,
    },
    /// Умолчание намеренно `Unknown`, а не `Validated`: снимок без
    /// записанного вердикта полноты не является проверенным.
    #[default]
    Unknown,
}

/// Сценарий, который владелец сравнивает в отчёте.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum OfferChoice {
    HoldToMaturity,
    ExerciseAtOffer { window: OfferWindowId },
}

/// Причина, по которой явно запрошенный сценарий оферты нельзя принять.
#[derive(Debug, Error, Clone, PartialEq, Eq)]
pub enum OfferChoiceError {
    #[error("окно оферты {window:?} отсутствует в графике")]
    UnknownWindow { window: OfferWindowId },
    #[error("условия расчёта окна оферты {window:?} неизвестны")]
    SettlementTermsUnknown { window: OfferWindowId },
    #[error("право окна оферты {window:?} не принадлежит владельцу")]
    RightIsNotHolders { window: OfferWindowId },
    /// Словарь источника классифицировал право как то, которого мы не
    /// моделируем. Это не то же самое, что право эмитента: сказать
    /// владельцу «право не ваше» про неизвестную нам конструкцию значит
    /// назвать отказ чужой причиной.
    #[error("право окна оферты {window:?} не моделируется")]
    RightNotModelled { window: OfferWindowId },
}

/// Перечислить все сценарии, доступные владельцу на дату среза.
#[must_use]
pub fn available_choices(schedule: &super::BondSchedule, as_of: Date) -> Vec<OfferChoice> {
    let mut choices = vec![OfferChoice::HoldToMaturity];
    choices.extend(
        schedule
            .offer_windows
            .iter()
            .filter_map(|terms| match terms.right {
                OfferRight::HolderPut => (terms.price_percent.is_some()
                    && terms.execution_date >= as_of)
                    .then_some(OfferChoice::ExerciseAtOffer {
                        window: terms.window,
                    }),
                OfferRight::HolderPutSettled | OfferRight::IssuerCall | OfferRight::Other => None,
            }),
    );
    choices
}

/// Проверить, что сценарий оферты ссылается на известное право и цену.
pub fn validate(
    choice: &OfferChoice,
    schedule: &super::BondSchedule,
) -> Result<(), OfferChoiceError> {
    match choice {
        OfferChoice::HoldToMaturity => Ok(()),
        OfferChoice::ExerciseAtOffer { window } => {
            let terms = schedule
                .offer_windows
                .iter()
                .find(|terms| terms.window == *window)
                .ok_or(OfferChoiceError::UnknownWindow { window: *window })?;

            match terms.right {
                OfferRight::HolderPut => {
                    if terms.price_percent.is_none() {
                        return Err(OfferChoiceError::SettlementTermsUnknown { window: *window });
                    }
                    Ok(())
                }
                OfferRight::HolderPutSettled | OfferRight::IssuerCall => {
                    Err(OfferChoiceError::RightIsNotHolders { window: *window })
                }
                OfferRight::Other => Err(OfferChoiceError::RightNotModelled { window: *window }),
            }
        }
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
        let instrument =
            InstrumentId(Uuid::parse_str("01234567-89ab-cdef-0123-456789abcdef").unwrap());
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
    fn distinct_execution_dates_are_valid() {
        let windows = [
            terms(date!(2026 - 12 - 01)),
            terms(date!(2027 - 12 - 01)),
        ];

        assert_eq!(validate_unique_windows(&windows), Ok(()));
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
    fn priced_terms(date: Date, right: OfferRight, price: &str) -> OfferWindowTerms {
        OfferWindowTerms {
            window: OfferWindowId::derive(InstrumentId::new_random(), date),
            right,
            execution_date: date,
            submission_start: None,
            submission_end: None,
            price_percent: Some(Dec::new(
                rust_decimal::Decimal::from_str_exact(price).unwrap(),
            )),
        }
    }

    fn schedule(windows: Vec<OfferWindowTerms>) -> crate::bond::BondSchedule {
        crate::bond::BondSchedule {
            offer_windows: windows,
            ..Default::default()
        }
    }

    #[test]
    fn hold_to_maturity_is_always_available() {
        let choices = available_choices(&schedule(Vec::new()), date!(2026 - 01 - 01));
        assert_eq!(choices, vec![OfferChoice::HoldToMaturity]);
    }

    #[test]
    fn only_an_exercisable_holder_put_is_listed() {
        let execution_date = date!(2026 - 12 - 01);
        let window = priced_terms(execution_date, OfferRight::HolderPut, "100");
        let expected = window.window;
        let choices = available_choices(&schedule(vec![window]), date!(2026 - 01 - 01));
        assert_eq!(
            choices,
            vec![
                OfferChoice::HoldToMaturity,
                OfferChoice::ExerciseAtOffer { window: expected }
            ]
        );
    }

    #[test]
    fn issuer_call_settled_put_missing_price_and_past_put_are_not_choices() {
        let windows = vec![
            priced_terms(date!(2026 - 02 - 01), OfferRight::IssuerCall, "100"),
            priced_terms(date!(2026 - 03 - 01), OfferRight::HolderPutSettled, "100"),
            terms(date!(2026 - 04 - 01)),
            priced_terms(date!(2025 - 12 - 01), OfferRight::HolderPut, "100"),
        ];
        assert_eq!(
            available_choices(&schedule(windows), date!(2026 - 01 - 01)),
            vec![OfferChoice::HoldToMaturity]
        );
    }

    #[test]
    fn validating_an_unknown_window_is_rejected() {
        let choice = OfferChoice::ExerciseAtOffer {
            window: OfferWindowId::new_random(),
        };
        assert!(matches!(
            validate(&choice, &schedule(Vec::new())),
            Err(OfferChoiceError::UnknownWindow { .. })
        ));
    }

    #[test]
    fn an_unmodelled_right_is_refused_by_its_own_reason() {
        // «Право не ваше» про конструкцию, которую словарь отнёс к прочему,
        // — отказ с чужой причиной: владелец пошёл бы чинить не то.
        let instrument = crate::ids::InstrumentId::new_random();
        let execution_date = date!(2027 - 01 - 15);
        let window = OfferWindowId::derive(instrument, execution_date);
        let schedule = super::super::BondSchedule {
            offer_windows: vec![OfferWindowTerms {
                window,
                right: OfferRight::Other,
                execution_date,
                submission_start: None,
                submission_end: None,
                price_percent: Some(Dec::new(rust_decimal::Decimal::from(100))),
            }],
            ..Default::default()
        };
        assert_eq!(
            validate(&OfferChoice::ExerciseAtOffer { window }, &schedule),
            Err(OfferChoiceError::RightNotModelled { window })
        );
    }

    #[test]
    fn validating_a_non_holder_right_is_rejected() {
        let window = priced_terms(date!(2026 - 12 - 01), OfferRight::IssuerCall, "100");
        let choice = OfferChoice::ExerciseAtOffer {
            window: window.window,
        };
        assert!(matches!(
            validate(&choice, &schedule(vec![window])),
            Err(OfferChoiceError::RightIsNotHolders { .. })
        ));
    }

    #[test]
    fn validating_a_holder_put_without_price_is_rejected() {
        let window = terms(date!(2026 - 12 - 01));
        let choice = OfferChoice::ExerciseAtOffer {
            window: window.window,
        };
        assert!(matches!(
            validate(&choice, &schedule(vec![window])),
            Err(OfferChoiceError::SettlementTermsUnknown { .. })
        ));
    }

    #[test]
    fn offer_choice_matches_are_exhaustive() {
        let expected_window = OfferWindowId::new_random();
        let choice = OfferChoice::ExerciseAtOffer {
            window: expected_window,
        };
        match choice {
            OfferChoice::HoldToMaturity => {}
            OfferChoice::ExerciseAtOffer { window } => assert_eq!(window, expected_window),
        }
    }

    #[test]
    fn offer_choice_error_matches_are_exhaustive() {
        let expected_window = OfferWindowId::new_random();
        let error = OfferChoiceError::UnknownWindow {
            window: expected_window,
        };
        match error {
            OfferChoiceError::UnknownWindow { window }
            | OfferChoiceError::SettlementTermsUnknown { window }
            | OfferChoiceError::RightIsNotHolders { window }
            | OfferChoiceError::RightNotModelled { window } => {
                assert_eq!(window, expected_window)
            }
        }
    }
}
