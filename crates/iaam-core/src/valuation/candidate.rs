//! Общий кандидат на оценку и порт выборки (E3.3, дизайн раздел 3).
//!
//! Два канала цены — биржевое наблюдение и утверждение владельца или
//! документа — приходят сюда одним типом. Исполнимость в кандидате
//! принадлежит источнику; всё, что вывела политика оценки, живёт в
//! [`SelectedPrice`] и в кандидат не попадает по построению.

use time::{Date, OffsetDateTime};

use crate::ids::{InstrumentId, SourceId};
use crate::money::CurrencyCode;
use crate::numeric::decimal::Dec;

use super::PriceQuality;

/// Откуда пришёл кандидат. Не выводится: канал известен в точке сборки.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PriceOrigin {
    /// Наблюдение из рыночного источника.
    Market { venue: String, kind: String },
    /// Цена, разобранная из отчёта или другого документа.
    ReportParsed { source: SourceId },
    /// Цена, утверждённая владельцем.
    OwnerAsserted,
}

/// Исполнимость по утверждению источника.
///
/// `Unknown` обязателен: владелец, вводя цену неликвида, не утверждает
/// ни того, что по ней можно выйти, ни того, что это цена закрытия.
/// Без этого варианта ручной канал вынужден лгать.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SourceExecutability {
    /// Источник утверждает доступную для выхода цену.
    Executable,
    /// Источник утверждает цену закрытия предыдущих торгов.
    IndicativePreviousClose,
    /// Источник не утверждает исполнимость цены.
    Unknown,
}

/// Способ выбора — почему дата наблюдения не совпала с датой оценки.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PriceSelection {
    /// Наблюдение относится ровно к дате оценки.
    AsObserved,
    /// Наблюдение перенесено с более ранней даты.
    CarriedForward { observed_on: Date, days: u16 },
    /// Значение унаследовано от старого правила и не переоценивается.
    LegacyDerived { quality: PriceQuality },
}

/// Свежесть — отдельная ось: цена бывает перенесённой и устаревшей.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PriceFreshness {
    /// Возраст цены не превышает порог свежести.
    Fresh,
    /// Возраст цены превышает обычный порог, но она ещё выбрана.
    Stale { days: u16 },
}

/// Почему позиция осталась без цены.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UncoveredReason {
    /// Для инструмента нет ни одного наблюдения.
    NoObservation,
    /// Все наблюдения старше предельного возраста.
    TooOld,
    /// Нельзя однозначно определить площадку.
    AmbiguousVenue,
    /// После отбора осталось несколько кандидатов.
    AmbiguousCandidate,
}

/// Совместимое с планом имя причины отсутствия покрытия.
pub type Uncovered = UncoveredReason;

/// Общий кандидат на оценку.
///
/// Исполнимость принадлежит источнику. Здесь намеренно нет
/// [`PriceSelection`]: перенос и устаревание являются выводами политики,
/// а не атрибутами наблюдения.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PriceCandidate {
    pub instrument: InstrumentId,
    pub price: Dec,
    pub currency: CurrencyCode,
    pub trade_date: Date,
    pub origin: PriceOrigin,
    pub executability: SourceExecutability,
}

/// Запрос выборки цены на дату оценки и в координате знания.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PriceQuery {
    pub instrument: InstrumentId,
    pub as_of: Date,
    pub knowledge_as_of: OffsetDateTime,
}

/// Выбранный кандидат с независимыми выводами политики.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SelectedPrice {
    pub candidate: PriceCandidate,
    pub selection: PriceSelection,
    pub freshness: PriceFreshness,
}

/// Результат разбора старого качества цены.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LegacyValuationOutcome {
    /// Старое событие можно представить как источник цены.
    Candidate(PriceCandidate),
    /// Старое событие содержит уже вычисленный результат политики.
    LegacyDerived(PriceQuality),
}

impl LegacyValuationOutcome {
    /// Возвращает кандидата, если legacy-качество допускает переоценку.
    #[must_use]
    pub const fn candidate(&self) -> Option<&PriceCandidate> {
        match self {
            Self::Candidate(candidate) => Some(candidate),
            Self::LegacyDerived(_) => None,
        }
    }

    /// Возвращает унаследованное качество, если оно терминально.
    #[must_use]
    pub const fn legacy(&self) -> Option<PriceQuality> {
        match self {
            Self::Candidate(_) => None,
            Self::LegacyDerived(quality) => Some(*quality),
        }
    }

    /// Извлекает кандидата, если он есть.
    #[must_use]
    pub fn into_candidate(self) -> Option<PriceCandidate> {
        match self {
            Self::Candidate(candidate) => Some(candidate),
            Self::LegacyDerived(_) => None,
        }
    }
}

/// Разбирает старое качество цены на происхождение и исполнимость.
///
/// `Executable`, `PreviousClose` и `OwnerEstimate` снова становятся
/// кандидатами. `CarriedForward` и `Stale` не становятся кандидатами:
/// legacy-событие хранит дату, к которой цену отнесли, но не исходную дату
/// наблюдения, поэтому повторная выборка отмыла бы старый вывод как свежий.
#[must_use]
pub fn candidate_from_legacy_valuation(
    quality: PriceQuality,
    mut candidate: PriceCandidate,
) -> LegacyValuationOutcome {
    match quality {
        PriceQuality::Executable => {
            candidate.executability = SourceExecutability::Executable;
            LegacyValuationOutcome::Candidate(candidate)
        }
        PriceQuality::PreviousClose => {
            candidate.executability = SourceExecutability::IndicativePreviousClose;
            LegacyValuationOutcome::Candidate(candidate)
        }
        PriceQuality::OwnerEstimate => {
            candidate.origin = PriceOrigin::OwnerAsserted;
            candidate.executability = SourceExecutability::Unknown;
            LegacyValuationOutcome::Candidate(candidate)
        }
        PriceQuality::CarriedForward | PriceQuality::Stale => {
            LegacyValuationOutcome::LegacyDerived(quality)
        }
    }
}

#[cfg(test)]
mod tests {
    use rust_decimal::Decimal;
    use time::macros::{date, datetime};

    use super::*;

    fn price() -> PriceCandidate {
        PriceCandidate {
            instrument: InstrumentId::new_random(),
            price: Dec::new(Decimal::from(281)),
            currency: CurrencyCode::Rub,
            trade_date: date!(2026 - 08 - 03),
            origin: PriceOrigin::ReportParsed {
                source: SourceId::new_random(),
            },
            executability: SourceExecutability::Executable,
        }
    }

    #[test]
    fn a_legacy_owner_estimate_becomes_an_owner_asserted_candidate() {
        let outcome = candidate_from_legacy_valuation(PriceQuality::OwnerEstimate, price());
        let candidate = outcome.candidate().expect("оценка владельца — кандидат");
        assert_eq!(candidate.origin, PriceOrigin::OwnerAsserted);
        assert_eq!(candidate.executability, SourceExecutability::Unknown);
    }

    #[test]
    fn a_legacy_carried_forward_price_is_never_re_derived() {
        let outcome = candidate_from_legacy_valuation(PriceQuality::CarriedForward, price());
        assert!(outcome.candidate().is_none());
        assert_eq!(
            outcome.legacy(),
            Some(PriceQuality::CarriedForward),
            "исходная дата наблюдения потеряна: переоценка выдала бы перенос за наблюдение"
        );
    }

    #[test]
    fn legacy_stale_price_is_never_re_derived() {
        let outcome = candidate_from_legacy_valuation(PriceQuality::Stale, price());
        assert!(outcome.candidate().is_none());
        assert_eq!(outcome.legacy(), Some(PriceQuality::Stale));
    }

    #[test]
    fn carried_forward_and_stale_are_independent_facts() {
        let selected = SelectedPrice {
            candidate: price(),
            selection: PriceSelection::CarriedForward {
                observed_on: date!(2026 - 07 - 01),
                days: 40,
            },
            freshness: PriceFreshness::Stale { days: 40 },
        };
        assert!(matches!(
            selected.selection,
            PriceSelection::CarriedForward { .. }
        ));
        assert!(matches!(selected.freshness, PriceFreshness::Stale { .. }));
    }

    #[test]
    fn source_quality_maps_to_source_executability() {
        let executable = candidate_from_legacy_valuation(PriceQuality::Executable, price());
        assert_eq!(
            executable.candidate().map(|candidate| candidate.executability),
            Some(SourceExecutability::Executable)
        );

        let previous_close =
            candidate_from_legacy_valuation(PriceQuality::PreviousClose, price());
        assert_eq!(
            previous_close
                .candidate()
                .map(|candidate| candidate.executability),
            Some(SourceExecutability::IndicativePreviousClose)
        );
    }

    #[test]
    fn price_query_keeps_evaluation_and_knowledge_coordinates() {
        let query = PriceQuery {
            instrument: InstrumentId::new_random(),
            as_of: date!(2026 - 08 - 26),
            knowledge_as_of: datetime!(2026 - 08 - 26 12:00 UTC),
        };
        assert_eq!(query.as_of, date!(2026 - 08 - 26));
        assert_eq!(query.knowledge_as_of, datetime!(2026 - 08 - 26 12:00 UTC));
    }
}
