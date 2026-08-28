//! Общий кандидат на оценку и порт выборки (E3.3, дизайн раздел 3).
//!
//! Два канала цены — биржевое наблюдение и утверждение владельца или
//! документа — приходят сюда одним типом. Исполнимость в кандидате
//! принадлежит источнику; всё, что вывела политика оценки, живёт в
//! [`SelectedPrice`] и в кандидат не попадает по построению.

use serde::{Deserialize, Serialize};
use time::{Date, OffsetDateTime};

use crate::ids::{InstrumentId, SourceId};
use crate::money::CurrencyCode;
use crate::numeric::decimal::Dec;

use super::PriceQuality;
/// Режим торгов, входящий в идентичность рыночного наблюдения.
///
/// Номер сессии отделяет основную торговлю от вечерней: одного кода
/// инструмента и доски недостаточно, чтобы связать цену с наблюдением НКД.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct Venue {
    pub board: String,
    pub session: i64,
}

/// Колонки рыночной цены MOEX, различаемые политикой оценки.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum PriceKind {
    Close,
    LegalClose,
    WeightedAverage,
    MarketPrice2,
    MarketPrice3,
    AdmittedQuote,
}

impl PriceKind {
    /// Каноническое имя колонки в проводном формате.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Close => "close",
            Self::LegalClose => "legal_close",
            Self::WeightedAverage => "weighted_average",
            Self::MarketPrice2 => "market_price_2",
            Self::MarketPrice3 => "market_price_3",
            Self::AdmittedQuote => "admitted_quote",
        }
    }
}

/// Откуда пришёл кандидат. Не выводится: канал известен в точке сборки.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PriceOrigin {
    /// Наблюдение из рыночного источника.
    Market { venue: Venue, kind: PriceKind },
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

/// Единица, в которой источник назвал цену (§10.2).
///
/// Третья ось наряду с полнотой и исполнимостью (ADR-0002), и, как они,
/// **атрибут наблюдения от источника**, а не вывод политики: основание
/// задаётся рынком и режимом торгов, из которого адаптер брал строку.
/// Вывести его правилом задним числом — то же смешение осей, которое
/// решение 0002 запрещает.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum QuotationBasis {
    /// Деньги за одну бумагу. Валюта числа — валюта наблюдения.
    MoneyPerUnit,
    /// Проценты непогашенного номинала. Само число **безразмерно**:
    /// денежная валюта приходит из валюты номинала, а не отсюда.
    PercentOfRemainingFace,
    /// Источник основания не доказал. Отказ при оценке, а не догадка.
    #[default]
    Unknown,
}

impl QuotationBasis {
    #[must_use]
    pub const fn code(self) -> &'static str {
        match self {
            Self::MoneyPerUnit => "money_per_unit",
            Self::PercentOfRemainingFace => "percent_of_remaining_face",
            Self::Unknown => "unknown",
        }
    }

    /// Разбор кода из хранилища. `None`, а не `Unknown`: неизвестный код —
    /// порча строки, и выдать её за недоказанное наблюдение значит
    /// спрятать порчу.
    #[must_use]
    pub fn from_code(code: &str) -> Option<Self> {
        [
            Self::MoneyPerUnit,
            Self::PercentOfRemainingFace,
            Self::Unknown,
        ]
        .into_iter()
        .find(|basis| basis.code() == code)
    }
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
    /// Единица цены. `#[serde(default)]` не нужен: `PriceCandidate`
    /// не сериализуется, он строится на каждом расчёте.
    pub basis: QuotationBasis,
    /// Признак, по которому основание выведено. Хранится рядом, а не
    /// восстанавливается по основанию: без него запись недоказуема
    /// при разборе аудита (§10.2).
    pub basis_evidence: String,
    /// Признак противоречит записанному основанию. Эффективное основание
    /// в таком кандидате уже `Unknown`, но причина отказа должна дойти
    /// до оценки позиции отдельно от недоказанности.
    pub basis_evidence_contradicts: bool,
    pub trade_date: Date,
    pub observed_at: OffsetDateTime,
    pub origin: PriceOrigin,
    pub executability: SourceExecutability,
}

/// Основание решения политики: все версии и пороги, способные
/// изменить его толкование (§6.6).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PriceProvenance {
    pub price_kind: Option<String>,
    pub origin: PriceOrigin,
    pub venue: Option<String>,
    /// Единица, в которой источник назвал цену. Без неё след аудита
    /// не объясняет, откуда взялась денежная стоимость позиции.
    pub quotation_basis: QuotationBasis,
    /// Признак, по которому основание выведено.
    pub basis_evidence: String,
    pub observed_at: OffsetDateTime,
    pub valuation_policy_version: u32,
    pub source_priority_version: u32,
    pub carry_forward_limit: u16,
    pub price_max_age: u16,
}

/// Выбранный кандидат с независимыми выводами политики и основанием.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SelectedPrice {
    pub candidate: PriceCandidate,
    pub selection: PriceSelection,
    pub freshness: PriceFreshness,
    pub provenance: PriceProvenance,
}

/// Запрос выборки цены на дату оценки и в координате знания.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PriceQuery {
    pub instrument: InstrumentId,
    pub as_of: Date,
    pub knowledge_as_of: OffsetDateTime,
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

    #[test]
    fn an_undecided_quotation_basis_is_unknown_not_money_per_unit() {
        // Строка, записанная до появления основания, недоказуема.
        // `MoneyPerUnit` по умолчанию объявил бы её доказанной (§4.9).
        assert_eq!(QuotationBasis::default(), QuotationBasis::Unknown);
    }

    #[test]
    fn every_quotation_basis_names_itself() {
        assert_eq!(QuotationBasis::MoneyPerUnit.code(), "money_per_unit");
        assert_eq!(
            QuotationBasis::PercentOfRemainingFace.code(),
            "percent_of_remaining_face"
        );
        assert_eq!(QuotationBasis::Unknown.code(), "unknown");
    }

    #[test]
    fn a_quotation_basis_survives_a_round_trip_through_its_code() {
        for basis in [
            QuotationBasis::MoneyPerUnit,
            QuotationBasis::PercentOfRemainingFace,
            QuotationBasis::Unknown,
        ] {
            assert_eq!(QuotationBasis::from_code(basis.code()), Some(basis));
        }
    }

    #[test]
    fn an_unrecognised_code_does_not_fall_back_to_a_basis() {
        // Неизвестный код из базы — это порча, а не `Unknown`: `Unknown`
        // означает «источник не доказал», а не «строку не прочитали».
        assert_eq!(QuotationBasis::from_code("percent"), None);
    }

    fn price() -> PriceCandidate {
        PriceCandidate {
            instrument: InstrumentId::new_random(),
            price: Dec::new(Decimal::from(281)),
            currency: CurrencyCode::Rub,
            basis: QuotationBasis::Unknown,
            basis_evidence: String::new(),
            basis_evidence_contradicts: false,
            trade_date: date!(2026 - 08 - 03),
            observed_at: datetime!(2026 - 08 - 03 18:00 UTC),
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
            provenance: PriceProvenance {
                price_kind: None,
                origin: PriceOrigin::ReportParsed {
                    source: SourceId::new_random(),
                },
                venue: None,
                quotation_basis: QuotationBasis::Unknown,
                basis_evidence: String::new(),
                observed_at: datetime!(2026 - 07 - 01 18:00 UTC),
                valuation_policy_version: 1,
                source_priority_version: 1,
                carry_forward_limit: 10,
                price_max_age: 30,
            },
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
            executable
                .candidate()
                .map(|candidate| candidate.executability),
            Some(SourceExecutability::Executable)
        );

        let previous_close = candidate_from_legacy_valuation(PriceQuality::PreviousClose, price());
        assert_eq!(
            previous_close
                .candidate()
                .map(|candidate| candidate.executability),
            Some(SourceExecutability::IndicativePreviousClose)
        );
    }

    #[test]
    fn into_candidate_extracts_only_revaluable_candidates() {
        let candidate = price();
        assert_eq!(
            LegacyValuationOutcome::Candidate(candidate.clone()).into_candidate(),
            Some(candidate)
        );
        assert_eq!(
            LegacyValuationOutcome::LegacyDerived(PriceQuality::Stale).into_candidate(),
            None
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
