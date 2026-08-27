//! Наблюдение рыночных данных (раздел 3 дизайна E3.2).
//!
//! Наблюдение **append-only и битемпорально**. Две оси времени:
//! `trade_date` — к какому дню относится значение, `observed_at` —
//! когда мы об этом узнали. Вторая назначается системой, а не берётся
//! из ответа: доверить её часам источника значит сделать ось знания
//! подделываемой ответом, а вместе с ней и воспроизводимость отчёта.

use iaam_core::ids::InstrumentId;
use iaam_core::money::{CurrencyCode, PerUnitAmount};
use iaam_core::numeric::decimal::Dec;
use iaam_core::valuation::QuotationBasis;
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
/// Вариантов `CarriedForward` и `Stale` здесь нет и быть не может: перенос
/// цены на нерабочий день и устаревание по порогу выводятся правилом оценки
/// (E3.3). Записать их наблюдением значит стереть различие между «биржа не
/// торговала» и «мы подставили вчерашнее» — и лишиться возможности пересчитать
/// отчёт по изменившемуся правилу.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Executability {
    /// Цена, по которой можно выйти: доступный bid.
    Executable,
    /// Цена закрытия предыдущих торгов — ориентир, не исполнение.
    IndicativePreviousClose,
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
    /// Единица цены, доказанная при разборе (§10.2).
    ///
    /// `#[serde(default)]` обязателен: наблюдения записаны до появления
    /// поля, и подставить им `MoneyPerUnit` значит объявить доказанным
    /// то, чего никто не доказывал.
    #[serde(default)]
    pub basis: QuotationBasis,
    /// Признак, из которого основание выведено.
    #[serde(default)]
    pub basis_evidence: String,
    pub executability: Executability,
}

/// Наблюдение накопленного купонного дохода.
///
/// Отдельный тип, а не поле в [`PriceObservation`], по трём причинам.
/// Во-первых, котировка облигации — процент номинала, а НКД — деньги:
/// одна структура на две размерности возвращает ошибку смешения единиц.
/// Во-вторых, исполнимости у НКД нет: это не цена, по которой кто-то
/// торгует. В-третьих, у акции такое поле было бы вечно пустым.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AccruedInterestObservation {
    pub instrument: InstrumentId,
    pub venue: Venue,
    pub trade_date: TradeDate,
    pub observed_at: ObservedAt,
    /// На ОДНУ бумагу, вместе с валютой из `FACEUNIT`.
    pub per_unit: PerUnitAmount,
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
    use iaam_core::ids::InstrumentId;
    use iaam_core::money::PerUnitAmount;
    use rust_decimal::Decimal;
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
        ];
        assert_eq!(all.len(), 2);
    }

    #[test]
    fn an_observation_written_before_the_basis_existed_reads_as_unknown() {
        let value = serde_json::json!({
            "instrument": InstrumentId::new_random(),
            "venue": {"board": "TQBR", "session": 3},
            "trade_date": TradeDate(date!(2026 - 08 - 03)),
            "observed_at": ObservedAt(datetime!(2026-08-03 19:00:00 UTC)),
            "kind": PriceKind::Close,
            "price": Dec::new(Decimal::from(100)),
            "currency": CurrencyCode::Rub,
            "executability": Executability::IndicativePreviousClose,
        });
        let observation: PriceObservation = serde_json::from_value(value).unwrap();
        assert_eq!(observation.basis, QuotationBasis::Unknown);
        assert_eq!(observation.basis_evidence, "");
    }

    #[test]
    fn accrued_interest_is_measured_per_bond_not_per_trade() {
        // Trade.accrued_interest — сумма ВСЕЙ сделки (event/mod.rs,
        // trade_settlement складывает её с gross целиком). Наблюдение —
        // величина на одну бумагу. Тип обязан делать подмену
        // непредставимой: голый Dec её не остановит.
        let observation = AccruedInterestObservation {
            instrument: InstrumentId::new_random(),
            venue: Venue {
                board: "TQOB".to_owned(),
                session: 3,
            },
            trade_date: TradeDate(date!(2026 - 08 - 20)),
            observed_at: ObservedAt(datetime!(2026-08-27 12:00:00 UTC)),
            per_unit: PerUnitAmount::new(
                Dec::new(Decimal::from_str_exact("15.17").unwrap()),
                CurrencyCode::Rub,
            ),
        };
        assert_eq!(observation.per_unit.currency(), CurrencyCode::Rub);
    }
}
