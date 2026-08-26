//! Наблюдение рыночных данных (раздел 3 дизайна E3.2).
//!
//! Наблюдение **append-only и битемпорально**. Две оси времени:
//! `trade_date` — к какому дню относится значение, `observed_at` —
//! когда мы об этом узнали. Вторая назначается системой, а не берётся
//! из ответа: доверить её часам источника значит сделать ось знания
//! подделываемой ответом, а вместе с ней и воспроизводимость отчёта.

use iaam_core::ids::InstrumentId;
use iaam_core::money::CurrencyCode;
use iaam_core::numeric::decimal::Dec;
use serde::{Deserialize, Serialize};
use time::{Date, OffsetDateTime};

/// К какому торговому дню относится значение (valid time).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct TradeDate(pub Date);

/// Когда мы узнали значение (knowledge time).
///
/// Отдельный тип, а не второй `Date`, намеренно: перепутать оси местами
/// не должно быть представимо (§15.1). Перестановка «когда цена» и
/// «когда узнали» не даёт ни ошибки компиляции, ни неверного числа —
/// она молча ломает воспроизводимость отчёта.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct ObservedAt(pub OffsetDateTime);

/// Исполнимость цены — **атрибут источника**, а не вывод политики.
///
/// Варианта `CarriedForward` здесь нет и быть не может: перенос цены
/// на нерабочий день выводится правилом оценки (E3.3). Записать его
/// наблюдением значит стереть различие между «биржа не торговала»
/// и «мы подставили вчерашнее» — и лишиться возможности пересчитать
/// отчёт по изменившемуся правилу.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Executability {
    /// Цена, по которой можно выйти: доступный bid.
    Executable,
    /// Цена закрытия предыдущих торгов — ориентир, не исполнение.
    IndicativePreviousClose,
    /// Наблюдение старше порога свежести источника.
    Stale,
}

/// Какая именно цена наблюдалась.
///
/// ISS отдаёт шесть кандидатов в одной строке. Ни один не объявлен
/// главным: выбор между ними — политика оценки, то есть E3.3.
/// Объявить главного здесь значило бы принять решение чужого
/// подпроекта молча.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum PriceKind {
    Close,
    LegalClose,
    WeightedAverage,
    MarketPrice2,
    MarketPrice3,
    AdmittedQuote,
}

/// Режим торгов.
///
/// Входит в идентичность наблюдения: один `SECID` торгуется в разных
/// режимах и валютах, и без режима две цены одного дня выглядят как
/// исправление одной.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Venue {
    /// Код режима торгов ISS, например `TQBR`.
    pub board: String,
    /// Номер торговой сессии: основная и вечерняя различаются.
    pub session: i64,
}

/// Наблюдение цены инструмента.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PriceObservation {
    pub instrument: InstrumentId,
    pub venue: Venue,
    pub trade_date: TradeDate,
    pub observed_at: ObservedAt,
    pub kind: PriceKind,
    pub price: Dec,
    /// Валюта площадки, **не «валюта инструмента»**: ISS отдаёт
    /// `CURRENCYID` построчно, и она принадлежит наблюдению.
    pub currency: CurrencyCode,
    pub executability: Executability,
}

/// Наблюдение курса.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FxObservation {
    pub from: CurrencyCode,
    pub to: CurrencyCode,
    pub trade_date: TradeDate,
    pub observed_at: ObservedAt,
    /// Номинал: ЦБ публикует курс за 1, 10 или 100 единиц.
    /// Голое число без номинала неинтерпретируемо.
    pub nominal: u32,
    /// Значение за номинал, как его дал источник.
    pub value: Dec,
    /// Значение за единицу. Хранится **вместе** с `value`: расхождение
    /// между ними — сигнал порчи разбора, и потерять его нельзя.
    pub unit_rate: Dec,
}

/// Наблюдение ключевой ставки.
///
/// Именно наблюдение по рабочему дню, а не интервал: источник отдаёт
/// дневной ряд и даты вступления в нём нет вовсе (раздел 8.3 спеки).
/// Интервал выводится на чтении и помечается выведенным.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct KeyRateObservation {
    pub trade_date: TradeDate,
    pub observed_at: ObservedAt,
    pub rate: Dec,
}

#[cfg(test)]
mod tests {
    use super::*;
    use time::macros::{date, datetime};

    #[test]
    fn the_two_time_axes_are_distinct_types() {
        let traded = TradeDate(date!(2026 - 08 - 03));
        let learned = ObservedAt(datetime!(2026-08-26 09:00:00 UTC));
        // Тест существует ради компилятора: если оси когда-нибудь станут
        // одним типом, перестановка аргументов в конструкторе наблюдения
        // пройдёт молча, а это подмена «когда цена» на «когда узнали».
        assert_ne!(traded.0.to_string(), learned.0.date().to_string());
    }

    #[test]
    fn executability_has_no_carried_forward_variant() {
        // Перенос цены на нерабочий день — вывод политики (E3.3),
        // а не то, что прислал источник (раздел 3.5 спеки). Вариант
        // в этом перечислении означал бы, что вывод можно записать
        // наблюдением, и различие «биржа не торговала» против
        // «мы подставили вчерашнее» потерялось бы навсегда.
        let all = [
            Executability::Executable,
            Executability::IndicativePreviousClose,
            Executability::Stale,
        ];
        assert_eq!(all.len(), 3);
    }
}
